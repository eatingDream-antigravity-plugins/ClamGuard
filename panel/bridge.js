// Lightweight SEP-1865 (2026-01-26) MCP Apps PostMessage bridge.

export const LATEST_PROTOCOL_VERSION = "2026-01-26";

function parseColorAlphaLum(raw) {
  const s = String(raw || "").trim();
  const low = s.toLowerCase();
  if (!s || low === "transparent" || low === "none" || low === "initial" || low === "unset") {
    return null;
  }
  const hex = s.match(/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i);
  if (hex) {
    const h = hex[1];
    const short = h.length <= 4;
    const dup = (i) => parseInt(short ? h[i] + h[i] : h.slice(i * 2, i * 2 + 2), 16);
    const a = h.length === 4 ? dup(3) / 255 : h.length === 8 ? dup(3) / 255 : 1;
    const lum = (0.2126 * dup(0) + 0.7152 * dup(1) + 0.0722 * dup(2)) / 255;
    return { a, lum };
  }
  const rgb = s.match(
    /^rgba?\(\s*(\d+(?:\.\d+)?)\s*[,\s]\s*(\d+(?:\.\d+)?)\s*[,\s]\s*(\d+(?:\.\d+)?)(?:\s*[,/]\s*(\d*(?:\.\d+)?)(%?))?\s*\)$/i
  );
  if (rgb) {
    let a = 1;
    if (rgb[4] != null && rgb[4] !== "") {
      const n = Number(rgb[4]);
      a = rgb[5] === "%" ? n / 100 : n;
      if (!Number.isFinite(a)) return null;
    }
    const lum = (0.2126 * Number(rgb[1]) + 0.7152 * Number(rgb[2]) + 0.0722 * Number(rgb[3])) / 255;
    return { a, lum };
  }
  if (/^(?:hsla?|hwb|lab|lch|oklab|oklch|color)\(/i.test(low)) {
    const m =
      low.match(/\/\s*(\d*(?:\.\d+)?)(%?)\s*\)$/) ||
      low.match(/^hsla?\(\s*[^,]+,\s*[^,]+,\s*[^,]+,\s*(\d*(?:\.\d+)?)(%?)\s*\)$/);
    if (m) {
      const n = Number(m[1]);
      const a = m[2] === "%" ? n / 100 : n;
      return Number.isFinite(a) ? { a, lum: null } : null;
    }
  }
  return { a: 1, lum: null };
}

export function parseColorLum(raw) {
  const c = parseColorAlphaLum(raw);
  return c && c.a >= 0.5 && c.lum !== null ? c.lum : null;
}

export function applyDocumentTheme(theme) {
  if (theme !== "light" && theme !== "dark") return;
  const el = document.documentElement;
  if (el.getAttribute("data-theme") !== theme) el.setAttribute("data-theme", theme);
  el.style.colorScheme = theme;
  const bg = `var(--color-background-primary,${theme === "light" ? "#FAF8F5" : "#141311"})`;
  el.style.backgroundColor = bg;
  if (document.body) document.body.style.backgroundColor = bg;
  const meta = document.querySelector?.('meta[name="color-scheme"]');
  if (meta && meta.getAttribute("content") !== theme) meta.setAttribute("content", theme);
}

export function applyHostStyleVariables(vars, target = document.documentElement) {
  if (!vars || typeof vars !== "object") return;
  for (const [k, v] of Object.entries(vars)) {
    if (!k.startsWith("--") || v == null) continue;
    const val = String(v).trim();
    if (k === "--color-background-primary") {
      const c = parseColorAlphaLum(val);
      if (!c || c.a < 0.9) continue;
    }
    target.style.setProperty(k, val);
  }
}

export function applyHostFonts(css) {
  if (!css) return;
  let style = document.getElementById("__mcp-host-fonts");
  if (!style) {
    style = document.createElement("style");
    style.id = "__mcp-host-fonts";
    document.head.appendChild(style);
  }
  if (style.textContent !== css) style.textContent = css;
}

export function applyHostContext(ctx) {
  if (!ctx || typeof ctx !== "object") return;
  if (ctx.styles?.variables) applyHostStyleVariables(ctx.styles.variables);
  if (ctx.styles?.css?.fonts) applyHostFonts(ctx.styles.css.fonts);
  let resolved = null;
  const raw = String(ctx.theme || "").trim().toLowerCase();
  if (/dark|night|black/.test(raw)) resolved = "dark";
  else if (/light|day|white/.test(raw)) resolved = "light";
  if (!resolved) {
    const bg =
      ctx.styles?.variables?.["--color-background-primary"] ||
      (typeof getComputedStyle === "function"
        ? getComputedStyle(document.documentElement).getPropertyValue("--color-background-primary")
        : "");
    const lum = parseColorLum(bg);
    if (lum !== null) resolved = lum < 0.45 ? "dark" : "light";
  }
  if (resolved) applyDocumentTheme(resolved);
}

export function App(appInfo, appCapabilities = {}, options = { autoResize: true }) {
  const caps = appCapabilities || {};
  const autoResize = options?.autoResize !== false;
  const target = typeof window !== "undefined" ? window.parent : null;
  const pending = new Map();
  let hostCaps = null;
  let hostCtx = null;
  let nextId = 1;
  let connected = false;
  let ro = null;

  const self = this || {};
  const post = (p) => target?.postMessage(p, "*");
  const reply = (id, result, error) =>
    post(error ? { jsonrpc: "2.0", id, error } : { jsonrpc: "2.0", id, result });
  const U = "ui/notifications/";

  async function onMessage(ev) {
    if (ev.source !== target) return;
    const m = ev.data;
    if (!m || typeof m !== "object" || m.jsonrpc !== "2.0") return;
    if (m.id !== undefined && !m.method) {
      const e = pending.get(m.id);
      if (!e) return;
      pending.delete(m.id);
      clearTimeout(e.t);
      if (m.error) e.rej(new Error(m.error.message || "RPC error"));
      else e.res(m.result);
      return;
    }
    if (m.id !== undefined && m.method) {
      try {
        if (m.method === "ping") return reply(m.id, {});
        if (m.method === "ui/resource-teardown") {
          const r = self.onteardown ? (await self.onteardown(m.params || {})) ?? {} : {};
          return reply(m.id, r);
        }
        reply(m.id, null, { code: -32601, message: "Not found" });
      } catch (err) {
        reply(m.id, null, { code: -32603, message: String(err) });
      }
    } else if (m.method) {
      const p = m.params || {};
      if (m.method === `${U}tool-input`) self.ontoolinput?.(p);
      else if (m.method === `${U}tool-result`) self.ontoolresult?.(p);
      else if (m.method === `${U}tool-cancelled`) self.ontoolcancelled?.(p);
      else if (m.method === `${U}host-context-changed`) {
        hostCtx = { ...hostCtx, ...p };
        self.onhostcontextchanged?.(p);
      } else if (m.method === "notifications/progress") {
        for (const e of pending.values()) e.reset();
      }
    }
  }

  function request(method, params, { timeoutMs = 75_000 } = {}) {
    const id = nextId++;
    return new Promise((res, rej) => {
      const mk = () =>
        setTimeout(() => {
          pending.delete(id);
          rej(new Error(`Timeout: ${method}`));
        }, timeoutMs);
      const entry = {
        res,
        rej,
        t: mk(),
        reset() {
          clearTimeout(entry.t);
          entry.t = mk();
        },
      };
      pending.set(id, entry);
      post({ jsonrpc: "2.0", id, method, params });
    });
  }

  const notification = (method, params) =>
    post(params !== undefined ? { jsonrpc: "2.0", method, params } : { jsonrpc: "2.0", method });

  function close() {
    connected = false;
    window.removeEventListener("message", onMessage);
    ro?.disconnect();
    ro = null;
    for (const e of pending.values()) {
      clearTimeout(e.t);
      e.rej(new Error("Closed"));
    }
    pending.clear();
  }

  Object.assign(self, {
    getHostCapabilities: () => hostCaps,
    getHostContext: () => hostCtx,
    request,
    notification,
    callServerTool: (params, opts = {}) =>
      request("tools/call", params, { timeoutMs: opts.timeout ?? 75_000 }),
    close,
    async connect() {
      if (connected) return;
      connected = true;
      window.addEventListener("message", onMessage);
      try {
        const r = await request(
          "ui/initialize",
          { appInfo, appCapabilities: caps, protocolVersion: LATEST_PROTOCOL_VERSION },
          { timeoutMs: 20_000 }
        );
        if (!r || typeof r !== "object") throw new Error("Invalid init");
        hostCaps = r.hostCapabilities;
        hostCtx = r.hostContext;
        applyHostContext(hostCtx);
        notification(`${U}initialized`);
        if (autoResize && hostCtx?.containerDimensions?.maxHeight != null && typeof ResizeObserver !== "undefined") {
          let q = false, lw = 0, lh = 0;
          const measure = () => {
            if (q) return;
            q = true;
            requestAnimationFrame(() => {
              q = false;
              const d = document.documentElement, prev = d.style.height;
              d.style.height = "max-content";
              const h = Math.ceil(d.getBoundingClientRect().height), w = Math.ceil(window.innerWidth);
              d.style.height = prev;
              if (w !== lw || h !== lh) {
                lw = w;
                lh = h;
                notification(`${U}size-changed`, { width: w, height: h });
              }
            });
          };
          measure();
          ro = new ResizeObserver(measure);
          ro.observe(document.documentElement);
          if (document.body) ro.observe(document.body);
        }
      } catch (err) {
        close();
        throw err;
      }
    },
  });
  return self;
}
