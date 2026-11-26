// Lightweight SEP-1865 (2026-01-26) MCP Apps PostMessage bridge.
export const LATEST_PROTOCOL_VERSION = "2026-01-26";
const BG = "--color-background-primary", J = "2.0", U = "ui/notifications/", lum3 = (r, g, b) => (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
const toA = (v, p) => { const a = p === "%" ? +v / 100 : +v; return Number.isFinite(a) ? a : null; };

function parseColorAlphaLum(raw) {
  const s = String(raw || "").trim(), low = s.toLowerCase();
  if (!s || /^(transparent|none|initial|unset)$/.test(low)) return null;
  let m = s.match(/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i);
  if (m) {
    const h = m[1], sh = h.length <= 4, d = (i) => parseInt(sh ? h[i] + h[i] : h.slice(i * 2, i * 2 + 2), 16);
    return { a: h.length === 4 || h.length === 8 ? d(3) / 255 : 1, lum: lum3(d(0), d(1), d(2)) };
  }
  m = s.match(/^rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)(?:\s*[,/]\s*([\d.]*)(%?))?\s*\)$/i);
  if (m) { const a = m[4] ? toA(m[4], m[5]) : 1; return a === null ? null : { a, lum: lum3(m[1], m[2], m[3]) }; }
  if (/^(hsla?|hwb|lab|lch|oklab|oklch|color)\(/i.test(low)) {
    m = low.match(/\/\s*([\d.]*)(%?)\s*\)$/) || low.match(/^hsla?\([^,]+,[^,]+,[^,]+,\s*([\d.]*)(%?)\s*\)$/);
    if (m) { const a = toA(m[1], m[2]); return a === null ? null : { a, lum: null }; }
  }
  return { a: 1, lum: null };
}

export const parseColorLum = (raw) => { const c = parseColorAlphaLum(raw); return c && c.a >= 0.5 && c.lum !== null ? c.lum : null; };

export function applyDocumentTheme(theme) {
  if (theme !== "light" && theme !== "dark") return;
  const d = document, el = d.documentElement, es = el.style, bg = `var(${BG},${theme === "light" ? "#FAF8F4" : "#141311"})`;
  if (el.getAttribute("data-theme") !== theme) el.setAttribute("data-theme", theme);
  es.colorScheme = theme; es.backgroundColor = bg;
  if (d.body) d.body.style.backgroundColor = bg;
  const meta = d.querySelector?.('meta[name="color-scheme"]');
  if (meta && meta.getAttribute("content") !== theme) meta.setAttribute("content", theme);
}

export function applyHostStyleVariables(vars, target = document.documentElement) {
  if (!vars || typeof vars !== "object") return;
  for (const [k, v] of Object.entries(vars)) {
    if (!k.startsWith("--") || v == null) continue;
    const val = String(v).trim();
    if (k === BG) { const c = parseColorAlphaLum(val); if (!c || c.a < 0.9) continue; }
    target.style.setProperty(k, val);
  }
}

export function applyHostFonts(css) {
  if (!css) return;
  const d = document, id = "__mcp-host-fonts";
  let st = d.getElementById(id);
  if (!st) { st = d.createElement("style"); st.id = id; d.head.appendChild(st); }
  if (st.textContent !== css) st.textContent = css;
}

export function applyHostContext(ctx) {
  if (!ctx || typeof ctx !== "object") return;
  const st = ctx.styles;
  if (st?.variables) applyHostStyleVariables(st.variables);
  if (st?.css?.fonts) applyHostFonts(st.css.fonts);
  const raw = String(ctx.theme || "").trim().toLowerCase();
  let t = /dark|night|black/.test(raw) ? "dark" : /light|day|white/.test(raw) ? "light" : null;
  if (!t) {
    const lum = parseColorLum(st?.variables?.[BG] || (typeof getComputedStyle < "u" ? getComputedStyle(document.documentElement).getPropertyValue(BG) : ""));
    if (lum !== null) t = lum < 0.45 ? "dark" : "light";
  }
  if (t) applyDocumentTheme(t);
}

export function App(appInfo, appCapabilities = {}, options = { autoResize: true }) {
  const caps = appCapabilities || {}, autoResize = options?.autoResize !== false, target = typeof window < "u" ? window.parent : null;
  const pending = new Map(), self = this || {};
  let hostCaps = null, hostCtx = null, nextId = 1, initOk = false, connP = null, ro = null;
  const post = (p) => target?.postMessage(p, "*");
  const reply = (id, result, error) => post(error ? { jsonrpc: J, id, error } : { jsonrpc: J, id, result });
  const evL = (m) => window[m + "EventListener"]("message", onMessage);

  async function onMessage(ev) {
    if (ev.source !== target) return;
    const m = ev.data; if (!m || typeof m !== "object" || m.jsonrpc !== J) return;
    const mm = m.method;
    if (m.id !== undefined && !mm) {
      const e = pending.get(m.id); if (!e) return;
      pending.delete(m.id); clearTimeout(e.t);
      m.error ? e.rej(new Error(m.error.message || "RPC error")) : e.res(m.result);
    } else if (m.id !== undefined) {
      try {
        if (mm === "ping") return reply(m.id, {});
        if (mm === "ui/resource-teardown") return reply(m.id, self.onteardown ? (await self.onteardown(m.params || {})) ?? {} : {});
        reply(m.id, null, { code: -32601, message: "Not found" });
      } catch (err) { reply(m.id, null, { code: -32603, message: String(err) }); }
    } else if (mm) {
      const p = m.params || {};
      if (mm.startsWith(U)) {
        const k = mm.slice(U.length);
        if (k === "host-context-changed") { hostCtx = { ...hostCtx, ...p }; self.onhostcontextchanged?.(p); }
        else self[{ "tool-input": "ontoolinput", "tool-result": "ontoolresult", "tool-cancelled": "ontoolcancelled" }[k]]?.(p);
      } else if (mm === "notifications/progress") for (const e of pending.values()) e.reset();
    }
  }

  function request(method, params, { timeoutMs = 75e3 } = {}) {
    const id = nextId++;
    return new Promise((res, rej) => {
      const mk = () => setTimeout(() => { pending.delete(id); rej(new Error(`Timeout: ${method}`)); }, timeoutMs);
      const e = { res, rej, t: mk(), reset() { clearTimeout(e.t); e.t = mk(); } };
      pending.set(id, e); post({ jsonrpc: J, id, method, params });
    });
  }
  const notification = (method, params) => post(params !== undefined ? { jsonrpc: J, method, params } : { jsonrpc: J, method });
  function close() {
    initOk = false; hostCaps = null; evL("remove"); ro?.disconnect(); ro = null;
    for (const e of pending.values()) { clearTimeout(e.t); e.rej(new Error("Closed")); }
    pending.clear();
  }

  return Object.assign(self, {
    isConnected: () => initOk,
    hasServerTools: () => Boolean(hostCaps?.serverTools && typeof hostCaps.serverTools === "object"),
    getHostCapabilities: () => hostCaps, getHostContext: () => hostCtx, request, notification, close,
    callServerTool: (params, opts = {}) => request("tools/call", params, { timeoutMs: opts.timeout ?? 75e3 }),
    connect() {
      if (initOk) return Promise.resolve();
      if (connP) return connP;
      evL("add");
      return (connP = (async () => {
        try {
          const r = await request("ui/initialize", { appInfo, appCapabilities: caps, protocolVersion: LATEST_PROTOCOL_VERSION }, { timeoutMs: 2e4 });
          if (!r || typeof r !== "object" || r.protocolVersion !== LATEST_PROTOCOL_VERSION) throw new Error("Unsupported protocol");
          hostCaps = r.hostCapabilities || {};
          hostCtx = r.hostContext; initOk = true; applyHostContext(hostCtx); notification(`${U}initialized`);
          if (autoResize && hostCtx?.containerDimensions?.maxHeight != null && typeof ResizeObserver < "u") {
            let q = false, lw = 0, lh = 0;
            const d = document.documentElement, ds = d.style, measure = () => {
              if (q) return; q = true;
              requestAnimationFrame(() => {
                q = false; const prev = ds.height; ds.height = "max-content";
                const h = Math.ceil(d.getBoundingClientRect().height), w = Math.ceil(window.innerWidth); ds.height = prev;
                if (w !== lw || h !== lh) { lw = w; lh = h; notification(`${U}size-changed`, { width: w, height: h }); }
              });
            };
            measure(); ro = new ResizeObserver(measure); ro.observe(d); if (document.body) ro.observe(document.body);
          }
        } catch (err) { close(); throw err; } finally { connP = null; }
      })());
    },
  });
}
