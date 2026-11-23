// Lightweight SEP-1865 (2026-01-26) MCP Apps PostMessage bridge.

export const LATEST_PROTOCOL_VERSION = "2026-01-26";

export function parseColorLum(raw) {
  const s = String(raw || "").trim();
  if (!s || s.toLowerCase() === "transparent") return null;
  const hex = s.match(/^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i);
  if (hex) {
    const h = hex[1];
    if (h.length === 4 && parseInt(h[3] + h[3], 16) / 255 < 0.5) return null;
    if (h.length === 8 && parseInt(h.slice(6, 8), 16) / 255 < 0.5) return null;
    const [r, g, b] = h.length <= 4
      ? [parseInt(h[0] + h[0], 16), parseInt(h[1] + h[1], 16), parseInt(h[2] + h[2], 16)]
      : [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
    return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
  }
  const rgb = s.match(
    /^rgba?\(\s*(\d+(?:\.\d+)?)\s*[,\s]\s*(\d+(?:\.\d+)?)\s*[,\s]\s*(\d+(?:\.\d+)?)(?:\s*[,/]\s*(\d*(?:\.\d+)?)(%?))?\s*\)$/i
  );
  if (!rgb) return null;
  if (rgb[4] != null && rgb[4] !== "") {
    const a = Number(rgb[4]);
    const alpha = rgb[5] === "%" ? a / 100 : a;
    if (!Number.isFinite(alpha) || alpha < 0.5) return null;
  }
  return (0.2126 * Number(rgb[1]) + 0.7152 * Number(rgb[2]) + 0.0722 * Number(rgb[3])) / 255;
}

function isOpaqueBgColor(raw) {
  const s = String(raw || "").trim().toLowerCase();
  if (!s || s === "transparent" || s === "none" || s === "initial" || s === "unset") return false;
  const hex = s.match(/^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i);
  if (hex) {
    const h = hex[1];
    if (h.length === 4) return parseInt(h[3] + h[3], 16) / 255 >= 0.9;
    if (h.length === 8) return parseInt(h.slice(6, 8), 16) / 255 >= 0.9;
    return true;
  }
  const rgb = s.match(
    /^rgba?\(\s*\d+(?:\.\d+)?\s*[,\s]\s*\d+(?:\.\d+)?\s*[,\s]\s*\d+(?:\.\d+)?(?:\s*[,/]\s*(\d*(?:\.\d+)?)(%?))?\s*\)$/i
  );
  if (rgb) {
    if (rgb[1] == null || rgb[1] === "") return true;
    const a = Number(rgb[1]);
    return Number.isFinite(a) && (rgb[2] === "%" ? a / 100 : a) >= 0.9;
  }
  if (/^(?:hsla?|hwb|lab|lch|oklab|oklch|color)\(/i.test(s)) {
    const slashAlpha = s.match(/\/\s*(\d*(?:\.\d+)?)(%?)\s*\)$/);
    if (slashAlpha) {
      const a = Number(slashAlpha[1]);
      return Number.isFinite(a) && (slashAlpha[2] === "%" ? a / 100 : a) >= 0.9;
    }
    const hsla4 = s.match(/^hsla?\(\s*[^,]+,\s*[^,]+,\s*[^,]+,\s*(\d*(?:\.\d+)?)(%?)\s*\)$/i);
    if (hsla4) {
      const a = Number(hsla4[1]);
      return Number.isFinite(a) && (hsla4[2] === "%" ? a / 100 : a) >= 0.9;
    }
  }
  return true;
}

export function applyDocumentTheme(theme) {
  if (theme !== "light" && theme !== "dark") return;
  const el = document.documentElement;
  if (el.getAttribute("data-theme") !== theme) el.setAttribute("data-theme", theme);
  el.style.colorScheme = theme;
  const bg = theme === "light" ? "var(--color-background-primary,#FAF8F5)" : "var(--color-background-primary,#141311)";
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
    if (k === "--color-background-primary" && !isOpaqueBgColor(val)) continue;
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
  if (raw.includes("dark") || raw.includes("night") || raw.includes("black")) resolved = "dark";
  else if (raw.includes("light") || raw.includes("day") || raw.includes("white")) resolved = "light";
  if (!resolved) {
    const bg = ctx.styles?.variables?.["--color-background-primary"] ||
      (typeof getComputedStyle === "function"
        ? getComputedStyle(document.documentElement).getPropertyValue("--color-background-primary")
        : "");
    const lum = parseColorLum(bg);
    if (lum !== null) resolved = lum < 0.45 ? "dark" : "light";
  }
  if (resolved) applyDocumentTheme(resolved);
}

export class App {
  #info;
  #caps;
  #autoResize;
  #hostCaps;
  #hostCtx;
  #nextId = 1;
  #pending = new Map();
  #connected = false;
  #target = window.parent;
  #ro = null;

  constructor(appInfo, appCapabilities = {}, options = { autoResize: true }) {
    this.#info = appInfo;
    this.#caps = appCapabilities || {};
    this.#autoResize = options?.autoResize !== false;
  }

  getHostCapabilities() {
    return this.#hostCaps;
  }

  getHostContext() {
    return this.#hostCtx;
  }

  #post(payload) {
    this.#target.postMessage(payload, "*");
  }

  #onMessage = (ev) => {
    if (ev.source !== this.#target) return;
    const m = ev.data;
    if (!m || typeof m !== "object" || m.jsonrpc !== "2.0") return;
    if (m.id !== undefined && !m.method) {
      const e = this.#pending.get(m.id);
      if (!e) return;
      this.#pending.delete(m.id);
      clearTimeout(e.t);
      if (m.error) e.rej(new Error(m.error.message || JSON.stringify(m.error)));
      else e.res(m.result);
      return;
    }
    if (m.id !== undefined && m.method) {
      this.#handleReq(m);
    } else if (m.method) {
      this.#handleNotif(m);
    }
  };

  async #handleReq(m) {
    try {
      if (m.method === "ping") return this.#post({ jsonrpc: "2.0", id: m.id, result: {} });
      if (m.method === "ui/resource-teardown") {
        const res = this.onteardown ? (await this.onteardown(m.params || {})) ?? {} : {};
        return this.#post({ jsonrpc: "2.0", id: m.id, result: res });
      }
      this.#post({
        jsonrpc: "2.0",
        id: m.id,
        error: { code: -32601, message: `Method not found: ${m.method}` },
      });
    } catch (err) {
      this.#post({
        jsonrpc: "2.0",
        id: m.id,
        error: { code: -32603, message: err?.message || String(err) },
      });
    }
  }

  #handleNotif(m) {
    const p = m.params || {};
    if (m.method === "ui/notifications/tool-input") this.ontoolinput?.(p);
    else if (m.method === "ui/notifications/tool-result") this.ontoolresult?.(p);
    else if (m.method === "ui/notifications/tool-cancelled") this.ontoolcancelled?.(p);
    else if (m.method === "ui/notifications/host-context-changed") {
      this.#hostCtx = { ...this.#hostCtx, ...p };
      this.onhostcontextchanged?.(p);
    } else if (m.method === "notifications/progress") {
      for (const e of this.#pending.values()) e.reset();
    }
  }

  request(method, params, { timeoutMs = 75_000 } = {}) {
    const id = this.#nextId++;
    return new Promise((res, rej) => {
      const mk = () =>
        setTimeout(() => {
          this.#pending.delete(id);
          rej(new Error(`Request timed out: ${method}`));
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
      this.#pending.set(id, entry);
      this.#post({ jsonrpc: "2.0", id, method, params });
    });
  }

  notification(method, params) {
    const payload = { jsonrpc: "2.0", method };
    if (params !== undefined) payload.params = params;
    this.#post(payload);
  }

  callServerTool(params, opts = {}) {
    return this.request("tools/call", params, { timeoutMs: opts.timeout ?? 75_000 });
  }

  #setupResize() {
    if (typeof ResizeObserver === "undefined") return;
    let q = false;
    let lw = 0;
    let lh = 0;
    const measure = () => {
      if (q) return;
      q = true;
      requestAnimationFrame(() => {
        q = false;
        const d = document.documentElement;
        const prev = d.style.height;
        d.style.height = "max-content";
        const h = Math.ceil(d.getBoundingClientRect().height);
        d.style.height = prev;
        const w = Math.ceil(window.innerWidth);
        if (w !== lw || h !== lh) {
          lw = w;
          lh = h;
          this.notification("ui/notifications/size-changed", { width: w, height: h });
        }
      });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(document.documentElement);
    if (document.body) ro.observe(document.body);
    this.#ro = ro;
  }

  async connect() {
    if (this.#connected) return;
    this.#connected = true;
    window.addEventListener("message", this.#onMessage);
    try {
      const r = await this.request(
        "ui/initialize",
        {
          appInfo: this.#info,
          appCapabilities: this.#caps,
          protocolVersion: LATEST_PROTOCOL_VERSION,
        },
        { timeoutMs: 20_000 }
      );
      if (!r || typeof r !== "object") throw new Error("Invalid ui/initialize result");
      this.#hostCaps = r.hostCapabilities;
      this.#hostCtx = r.hostContext;
      applyHostContext(this.#hostCtx);
      this.notification("ui/notifications/initialized");
      // Only emit size-changed in inline conversation frames (aux_pane/global ignore reported sizes).
      if (this.#autoResize && this.#hostCtx?.containerDimensions?.maxHeight != null) {
        this.#setupResize();
      }
    } catch (err) {
      this.close();
      throw err;
    }
  }

  close() {
    this.#connected = false;
    window.removeEventListener("message", this.#onMessage);
    if (this.#ro) {
      this.#ro.disconnect();
      this.#ro = null;
    }
    for (const e of this.#pending.values()) {
      clearTimeout(e.t);
      e.rej(new Error("MCP App connection closed"));
    }
    this.#pending.clear();
  }
}
