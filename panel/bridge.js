// SEP-1865 (2026-01-26) MCP Apps PostMessage bridge.
export const LATEST_PROTOCOL_VERSION = "2026-01-26";
const BG = "--color-background-primary", J = "2.0", U = "ui/notifications/", E = (s) => new Error(s);
const lum3 = (r, g, b) => (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255, toA = (v, p) => { const a = p === "%" ? +v / 100 : +v; return Number.isFinite(a) ? a : null; };
const D = () => (typeof document < "u" ? document : null), isO = (v) => v && typeof v === "object", isS = (v) => typeof v === "string";

function parseColorAlphaLum(raw) {
  const s = String(raw || "").trim(), low = s.toLowerCase();
  if (!s || /^(transparent|none|initial|unset)$/.test(low)) return null;
  let m = s.match(/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i);
  if (m) {
    const h = m[1], sh = h.length <= 4, d = (i) => parseInt(sh ? h[i] + h[i] : h.slice(i * 2, i * 2 + 2), 16);
    return { a: h.length === 4 || h.length === 8 ? d(3) / 255 : 1, lum: lum3(d(0), d(1), d(2)) };
  }
  m = s.match(/^rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)(?:\s*[,/]\s*([\d.]*)(%?))?\s*\)$/i);
  if (m) { const a = m[4] ? toA(m[4], m[5]) : 1; return a === null ? null : { a, lum: lum3(+m[1], +m[2], +m[3]) }; }
  if (/^(hsla?|hwb|lab|lch|oklab|oklch|color)\(/i.test(low)) {
    m = low.match(/\/\s*([\d.]*)(%?)\s*\)$/) || low.match(/^hsla?\([^,]+,[^,]+,[^,]+,\s*([\d.]*)(%?)\s*\)$/);
    return m ? ((m = toA(m[1], m[2])) === null ? null : { a: m, lum: null }) : { a: 1, lum: null };
  }
  return low === "black" ? { a: 1, lum: 0 } : low === "white" ? { a: 1, lum: 1 } : null;
}

export const parseColorLum = (raw) => { const c = parseColorAlphaLum(raw); return c && c.a >= 0.5 && c.lum !== null ? c.lum : null; };

export function applyDocumentTheme(theme, doc = D()) {
  const el = doc?.documentElement;
  if ((theme !== "light" && theme !== "dark") || !el) return;
  const es = el.style, bg = `var(${BG},${theme === "light" ? "#FAF8F4" : "#141311"})`;
  if (el.getAttribute("data-theme") !== theme) el.setAttribute("data-theme", theme);
  es.colorScheme = theme; es.backgroundColor = bg;
  if (doc.body) doc.body.style.backgroundColor = bg;
  const meta = doc.querySelector?.('meta[name="color-scheme"]');
  if (meta && meta.getAttribute("content") !== theme) meta.setAttribute("content", theme);
}

export function applyHostStyleVariables(vars, target = D()?.documentElement) {
  if (!isO(vars) || !target) return;
  for (const [k, v] of Object.entries(vars)) {
    if (!k.startsWith("--") || v == null) continue;
    const val = String(v).trim();
    if (!val) continue;
    if (k === BG) { const c = parseColorAlphaLum(val); if (!c || c.a < 0.9) continue; }
    target.style.setProperty(k, val);
  }
}

export function applyHostFonts(css, doc = D()) {
  if (!css || !doc?.head) return;
  const id = "__mcp-host-fonts";
  let st = doc.getElementById(id);
  if (!st) { st = doc.createElement("style"); st.id = id; doc.head.appendChild(st); }
  if (st.textContent !== css) st.textContent = css;
}

export function applyHostContext(ctx, doc = D()) {
  if (!isO(ctx)) return;
  const st = ctx.styles, el = doc?.documentElement;
  if (st?.variables && el) applyHostStyleVariables(st.variables, el);
  if (st?.css?.fonts) applyHostFonts(st.css.fonts, doc);
  const raw = String(ctx.theme || "").trim().toLowerCase();
  let t = /dark|night|black/.test(raw) ? "dark" : /light|day|white/.test(raw) ? "light" : null;
  if (!t) {
    const lum = parseColorLum(st?.variables?.[BG] || (el && typeof getComputedStyle < "u" ? getComputedStyle(el).getPropertyValue(BG) : ""));
    if (lum !== null) t = lum < 0.45 ? "dark" : "light";
  }
  if (t) applyDocumentTheme(t, doc);
}

export function App(appInfo, appCapabilities = {}, options = {}) {
  const win = options.window ?? (typeof window < "u" ? window : null), doc = options.document ?? D();
  const caps = appCapabilities || {}, target = options.target ?? (win?.parent && win.parent !== win ? win.parent : null);
  const pending = new Map(), self = this || {}, st = setTimeout, ct = clearTimeout;
  let hostCaps = null, hostCtx = null, nextId = 1, initOk = false, epoch = 0, connP = null, on = false;

  const post = (p) => { if (!target?.postMessage) throw E("Parent unavailable"); target.postMessage(p, "*"); };
  const reply = (id, result, error) => post(error ? { jsonrpc: J, id, error } : { jsonrpc: J, id, result });
  const listen = (add) => { if (add !== on && win) { win[(add ? "add" : "remove") + "EventListener"]("message", onMessage); on = add; } };

  async function onMessage(ev) {
    if (!target || ev?.source !== target) return;
    const m = ev.data; if (!isO(m) || m.jsonrpc !== J) return;
    const mm = m.method;
    if (m.id !== undefined && !mm) {
      const e = pending.get(m.id); if (!e) return;
      pending.delete(m.id); e.c();
      m.error ? e.rej(E(m.error.message || "RPC error")) : e.res(m.result);
    } else if (m.id !== undefined && isS(mm)) {
      try {
        if (mm === "ping") return reply(m.id, {});
        if (mm === "ui/resource-teardown") return reply(m.id, self.onteardown ? (await self.onteardown(m.params || {})) ?? {} : {});
        reply(m.id, null, { code: -32601, message: "Not found" });
      } catch (err) { reply(m.id, null, { code: -32603, message: String(err) }); }
    } else if (isS(mm)) {
      const p = m.params || {};
      if (mm.startsWith(U)) {
        const k = mm.slice(U.length);
        if (k === "host-context-changed") { hostCtx = { ...hostCtx, ...p }; applyHostContext(hostCtx, doc); self.onhostcontextchanged?.(p, hostCtx); }
        else self[{ "tool-input": "ontoolinput", "tool-result": "ontoolresult", "tool-cancelled": "ontoolcancelled" }[k]]?.(p);
      } else if (mm === "notifications/progress" && p.progressToken != null) {
        for (const e of pending.values()) if (e.tok !== null && e.tok === p.progressToken) e.r();
      }
    }
  }

  function request(method, params, { timeoutMs = 75e3, maxTimeoutMs } = {}) {
    const id = nextId++, rt = params?._meta?.progressToken, tok = isS(rt) || (typeof rt === "number" && Number.isFinite(rt)) ? rt : null;
    const hardMs = maxTimeoutMs > 0 ? Math.max(timeoutMs, maxTimeoutMs) : timeoutMs * 4;
    return new Promise((res, rej) => {
      const exp = () => { if (pending.delete(id)) { e.c(); rej(E(`Timeout: ${method}`)); } }, mk = () => st(exp, timeoutMs);
      const e = { res, rej, tok, t: mk(), h: st(exp, hardMs), c() { ct(e.t); ct(e.h); }, r() { ct(e.t); e.t = mk(); } };
      pending.set(id, e);
      try { post({ jsonrpc: J, id, method, params }); } catch (err) { e.c(); pending.delete(id); rej(err); }
    });
  }

  const notification = (method, params) => post(params !== undefined ? { jsonrpc: J, method, params } : { jsonrpc: J, method });
  const reset = (why) => {
    initOk = false; hostCaps = hostCtx = null; listen(false);
    for (const e of pending.values()) { e.c(); e.rej(E(why)); }
    pending.clear();
  };
  const close = () => { epoch += 1; connP = null; reset("Closed"); };

  async function callServerTool(params, opts = {}) {
    if (!initOk) throw E("Not connected");
    if (!self.hasServerTools()) throw E("No serverTools");
    return request("tools/call", params, { timeoutMs: opts.timeout ?? 75e3, maxTimeoutMs: opts.maxTimeout });
  }

  function connect(opts = {}) {
    if (initOk) return Promise.resolve();
    if (connP) return connP;
    const myE = epoch, maxR = Math.max(0, +(opts.retries ?? 0)), delay = Math.max(0, +(opts.retryDelayMs ?? 200)), tMs = +(opts.timeoutMs ?? 2e4);
    const p = (async () => {
      let last;
      for (let i = 0; i <= maxR; i += 1) {
        if (myE !== epoch) throw E("Closed");
        listen(true);
        try {
          const r = await request("ui/initialize", { appInfo, appCapabilities: caps, protocolVersion: LATEST_PROTOCOL_VERSION }, { timeoutMs: tMs });
          if (myE !== epoch) throw E("Closed");
          if (!isO(r) || r.protocolVersion !== LATEST_PROTOCOL_VERSION) throw E("Unsupported protocol");
          hostCaps = r.hostCapabilities || {}; hostCtx = r.hostContext || null; initOk = true;
          applyHostContext(hostCtx, doc); notification(`${U}initialized`);
          return;
        } catch (err) {
          last = err;
          if (myE !== epoch) throw E("Closed");
          reset("Retry");
          if (i < maxR) { await new Promise((r) => st(r, delay)); if (myE !== epoch) throw E("Closed"); }
        }
      }
      throw last;
    })().finally(() => { if (connP === p) connP = null; });
    return (connP = p);
  }

  return Object.assign(self, {
    isConnected: () => initOk,
    hasServerTools: () => Boolean(isO(hostCaps?.serverTools)),
    getHostCapabilities: () => hostCaps, getHostContext: () => hostCtx, request, notification, close, connect, callServerTool,
  });
}
