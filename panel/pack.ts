import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import type * as Esbuild from "esbuild";
import { assertPanelInvariants } from "./kit/guard.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);

const entryTs = path.join(__dirname, "app.ts");
const kitTokensPath = path.join(__dirname, "kit", "tokens.css");
const cssPath = path.join(__dirname, "styles.css");
const templatePath = path.join(__dirname, "template.html");
const distDir = path.join(__dirname, "dist");
const outHtml = path.join(distDir, "panel.html");
const BOOT_TOKEN = "/*__MAC_CLAMSHELL_BOOT__*/";
const MODULES = ["kit/bridge.ts", "polling.ts", "brightness.ts", "model.ts"];
const SIZE_FAIL_BYTES = 65_536;
const SIZE_WARN_BYTES = 57_344;

function tryLoadEsbuild(): typeof Esbuild | null {
  if (process.env.MAC_CLAMSHELL_BUNDLER === "bun") return null;
  for (const mod of [process.env.ESBUILD_PATH, "esbuild"].filter((m): m is string => Boolean(m))) {
    try {
      return require(mod) as typeof Esbuild;
    } catch {}
  }
  return null;
}

function tryBunBundle(): string | null {
  let tmpDir = "";
  try {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "mc-pack-"));
    for (const f of [...MODULES, "app.ts"]) {
      const dst = path.join(tmpDir, f);
      fs.mkdirSync(path.dirname(dst), { recursive: true });
      fs.writeFileSync(dst, fs.readFileSync(path.join(__dirname, f), "utf8"), "utf8");
    }
    const out = execFileSync(
      "bun",
      ["build", path.join(tmpDir, "app.ts"), "--bundle", "--minify", "--format=iife", "--target=browser"],
      { encoding: "utf8", maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] }
    );
    return out.trim() ? out.trim() : null;
  } catch {
    return null;
  } finally {
    if (tmpDir) try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch {}
  }
}

function tryBunCss(combinedCss: string): string | null {
  let tmpDir = "";
  try {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "mc-css-"));
    const tmpCss = path.join(tmpDir, "combined.css");
    fs.writeFileSync(tmpCss, combinedCss, "utf8");
    const out = execFileSync(
      "bun",
      ["build", tmpCss, "--minify"],
      { encoding: "utf8", maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] }
    );
    return out.trim() ? out.trim() : null;
  } catch {
    return null;
  } finally {
    if (tmpDir) try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch {}
  }
}

function fallbackMinifyCss(css: string): string {
  return css
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\s+/g, " ")
    .replace(/\s*([{}:;,>+~])\s*/g, "$1")
    .replace(/;}/g, "}")
    .trim();
}

function cleanCss(css: string): string {
  return css
    .replace(/--buncss-[a-z0-9-]+:[^;}"']+;?/gi, "")
    .replace(/--([a-z0-9-]+):\s+/gi, "--$1:")
    .replace(/,\s+/g, ",")
    .replace(/:\s+/g, ":")
    .replace(/\s*\{\s*/g, "{")
    .replace(/;\s*/g, ";")
    .replace(/;}/g, "}")
    .replace(/<\/style/gi, "<\\/style")
    .trim();
}

const rawCss = `${fs.readFileSync(kitTokensPath, "utf8")}\n${fs.readFileSync(cssPath, "utf8")}`;
const esbuild = tryLoadEsbuild();
let jsCode: string;
let cssCode: string;
let bundler = "bun";

if (esbuild) {
  bundler = "esbuild";
  const result = await esbuild.build({
    entryPoints: [entryTs],
    bundle: true,
    minify: true,
    treeShaking: true,
    charset: "utf8",
    legalComments: "none",
    format: "iife",
    platform: "browser",
    target: ["es2022"],
    write: false,
  });
  jsCode = result.outputFiles[0].text.trim().replace(/<\/script/gi, "<\\/script");
  const minCss = await esbuild.transform(rawCss, {
    loader: "css",
    minify: true,
    charset: "utf8",
    legalComments: "none",
  });
  cssCode = cleanCss(minCss.code);
} else {
  const bunJs = tryBunBundle();
  if (!bunJs) {
    throw new Error("Neither esbuild nor bun is available to bundle TypeScript panel sources");
  }
  jsCode = bunJs.replace(/<\/script/gi, "<\\/script");
  cssCode = cleanCss(tryBunCss(rawCss) || fallbackMinifyCss(rawCss));
}

const template = fs
  .readFileSync(templatePath, "utf8")
  .replace(/>\s+</g, "><")
  .replace(/\r?\n/g, "")
  .replace(/(<meta name="app-version"[^>]*><!-- x-release-please-version -->)/, "\n$1\n")
  .trim();

if (!template.includes(BOOT_TOKEN)) {
  throw new Error(`template.html is missing ${BOOT_TOKEN} SSR placeholder`);
}

const html = template
  .replace("/*__INLINE_CSS__*/", () => cssCode)
  .replace("/*__INLINE_JS__*/", () => jsCode);

const report = assertPanelInvariants(html, {
  maxBytes: SIZE_FAIL_BYTES,
  hardMaxBytes: SIZE_FAIL_BYTES,
  warnBytes: SIZE_WARN_BYTES,
  bootPlaceholder: BOOT_TOKEN,
});
for (const w of report.warnings) console.warn(`WARN: ${w}`);

const jsBytes = Buffer.byteLength(jsCode, "utf8");
const cssBytes = Buffer.byteLength(cssCode, "utf8");
const totalBytes = report.bytes;

fs.mkdirSync(distDir, { recursive: true });
fs.writeFileSync(outHtml, html, "utf8");

console.log(
  `Built ${outHtml} (${totalBytes} B / ${(totalBytes / 1024).toFixed(1)} KiB total: JS ${jsBytes} B + CSS ${cssBytes} B, bundler=${bundler})`
);
