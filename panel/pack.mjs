import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);

const entryJs = path.join(__dirname, "app.js");
const cssPath = path.join(__dirname, "styles.css");
const templatePath = path.join(__dirname, "template.html");
const distDir = path.join(__dirname, "dist");
const outHtml = path.join(distDir, "panel.html");
const BOOT_TOKEN = "/*__MAC_CLAMSHELL_BOOT__*/";

function compactHtmlTemplates(src) {
  return src
    .replace(/>\n[ \t]*</g, "><")
    .replace(/>\n[ \t]*\$\{/g, ">${")
    .replace(/\}\n[ \t]*</g, "}<")
    .replace(/\}\n[ \t]*\$\{/g, "}${");
}

function tryLoadEsbuild() {
  for (const mod of [process.env.ESBUILD_PATH, "esbuild"].filter(Boolean)) {
    try { return require(mod); } catch {}
  }
  return null;
}

function tryBunBundle() {
  let tmpDir = "";
  try {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "mc-pack-"));
    for (const f of ["bridge.js", "polling.js", "brightness.js"]) {
      fs.writeFileSync(path.join(tmpDir, f), fs.readFileSync(path.join(__dirname, f), "utf8"), "utf8");
    }
    const tmpEntry = path.join(tmpDir, "app.js");
    fs.writeFileSync(tmpEntry, compactHtmlTemplates(fs.readFileSync(entryJs, "utf8")), "utf8");
    const out = execFileSync(
      "bun",
      ["build", tmpEntry, "--bundle", "--minify", "--format=iife", "--target=browser"],
      { encoding: "utf8", maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] }
    );
    return out.trim() ? out.trim() : null;
  } catch {
    return null;
  } finally {
    if (tmpDir) try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch {}
  }
}

function tryBunCss() {
  try {
    const out = execFileSync(
      "bun",
      ["build", cssPath, "--minify"],
      { encoding: "utf8", maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] }
    );
    return out.trim() ? out.trim() : null;
  } catch {
    return null;
  }
}

function fallbackMinifyCss(css) {
  return css
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\s+/g, " ")
    .replace(/\s*([{}:;,>+~])\s*/g, "$1")
    .replace(/;}/g, "}")
    .replace(/--([a-z0-9-]+):\s+/gi, "--$1:")
    .trim();
}

function fallbackBundleJs() {
  const parts = ["bridge.js", "polling.js", "brightness.js"].map((f) =>
    fs.readFileSync(path.join(__dirname, f), "utf8").replace(/^\s*export\s+/gm, "")
  );
  const appSrc = compactHtmlTemplates(
    fs.readFileSync(entryJs, "utf8").replace(/^\s*import\s+[\s\S]*?from\s+["']\.\/[^"']+["'];?\s*/gm, "")
  );
  return `(()=>{${parts.join("\n")}\n${appSrc}})();`;
}

const rawCss = fs.readFileSync(cssPath, "utf8");
const esbuild = tryLoadEsbuild();
let jsCode;
let cssCode;
let bundler = "fallback";

if (esbuild) {
  bundler = "esbuild";
  const compactHtmlTemplatesPlugin = {
    name: "compact-html-templates",
    setup(build) {
      build.onLoad({ filter: /\/app\.js$/ }, async (args) => {
        const src = await fs.promises.readFile(args.path, "utf8");
        return { contents: compactHtmlTemplates(src), loader: "js" };
      });
    },
  };
  const result = await esbuild.build({
    entryPoints: [entryJs],
    bundle: true,
    minify: true,
    treeShaking: true,
    charset: "utf8",
    legalComments: "none",
    format: "iife",
    platform: "browser",
    target: ["es2022"],
    plugins: [compactHtmlTemplatesPlugin],
    write: false,
  });
  jsCode = result.outputFiles[0].text.trim().replace(/<\/script/gi, "<\\/script");
  const minCss = await esbuild.transform(rawCss, {
    loader: "css",
    minify: true,
    charset: "utf8",
    legalComments: "none",
  });
  cssCode = minCss.code.trim().replace(/--([a-z0-9-]+):\s+/gi, "--$1:").replace(/<\/style/gi, "<\\/style");
} else {
  const bunJs = tryBunBundle();
  if (bunJs) {
    bundler = "bun";
    jsCode = bunJs.replace(/<\/script/gi, "<\\/script");
  } else {
    jsCode = fallbackBundleJs().replace(/<\/script/gi, "<\\/script");
  }
  cssCode = (tryBunCss() || fallbackMinifyCss(rawCss))
    .replace(/--([a-z0-9-]+):\s+/gi, "--$1:")
    .replace(/<\/style/gi, "<\\/style");
}

const template = fs
  .readFileSync(templatePath, "utf8")
  .replace(/>\s+</g, "><")
  .trim();

if (!template.includes(BOOT_TOKEN)) {
  throw new Error(`template.html is missing ${BOOT_TOKEN} SSR placeholder`);
}

const html = template
  .replace("/*__INLINE_CSS__*/", () => cssCode)
  .replace("/*__INLINE_JS__*/", () => jsCode);

fs.mkdirSync(distDir, { recursive: true });
fs.writeFileSync(outHtml, html, "utf8");

const totalBytes = Buffer.byteLength(html, "utf8");
const jsKb = (Buffer.byteLength(jsCode, "utf8") / 1024).toFixed(1);
const cssKb = (Buffer.byteLength(cssCode, "utf8") / 1024).toFixed(1);
const totalKb = (totalBytes / 1024).toFixed(1);
console.log(`Built ${outHtml} (${totalKb} KiB total: JS ${jsKb} KiB + CSS ${cssKb} KiB, bundler=${bundler})`);
