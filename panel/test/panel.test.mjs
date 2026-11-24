import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  BG_POLL_INTERVAL_FAST_MS,
  BG_POLL_INTERVAL_MED_MS,
  BG_POLL_INTERVAL_SLOW_MS,
  BG_POLL_INTERVAL_ULTRA_SLOW_MS,
  POINTER_STALE_MS,
  POLL_INTERVAL_FAST_MS,
  POLL_INTERVAL_MAX_BACKOFF_MS,
  POLL_INTERVAL_MED_MS,
  POLL_INTERVAL_SLOW_MS,
  POLL_INTERVAL_ULTRA_SLOW_MS,
  REVEAL_THROTTLE_MS,
  attachRevealRefresh,
  computeAdaptiveIntervalMs,
  createAdaptivePoller,
} from "../polling.js";
import {
  BRIGHTNESS_DEBOUNCE_MS,
  clampPercent,
  createBrightnessController,
} from "../brightness.js";
import { applyHostStyleVariables, parseColorLum } from "../bridge.js";
import { computeOptimisticState, describeGuardStatus, resolvePanelLang } from "../model.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PANEL_DIR = path.resolve(__dirname, "..");

describe("Mac Clamshell Panel: Adaptive Polling, Brightness Coalescing & Editorial UI", () => {
  it("adapts interval from net RTT in foreground and continues background polling when hidden", async () => {
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 40, hidden: false }), POLL_INTERVAL_FAST_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 220, hidden: false }), POLL_INTERVAL_MED_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 450, hidden: false }), POLL_INTERVAL_SLOW_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 950, hidden: false }), POLL_INTERVAL_ULTRA_SLOW_MS);

    assert.equal(computeAdaptiveIntervalMs({ rttMs: 40, hidden: true }), BG_POLL_INTERVAL_FAST_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 220, hidden: true }), BG_POLL_INTERVAL_MED_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 450, hidden: true }), BG_POLL_INTERVAL_SLOW_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 950, hidden: true }), BG_POLL_INTERVAL_ULTRA_SLOW_MS);
    assert.equal(
      computeAdaptiveIntervalMs({ rttMs: 450, consecutiveErrors: 4, hidden: false }),
      POLL_INTERVAL_MAX_BACKOFF_MS
    );

    let hidden = false;
    let wallClock = 10_000;
    let perfClock = 1000;
    let tid = 1;
    const timers = new Map();
    const fetchCalls = [];
    let pending = null;

    const poller = createAdaptivePoller({
      isHidden: () => hidden,
      now: () => wallClock,
      perfNow: () => perfClock,
      setTimer: (fn, ms) => {
        const id = tid++;
        timers.set(id, { fn, ms });
        return id;
      },
      clearTimer: (id) => timers.delete(id),
      fetcher: (opts) => {
        fetchCalls.push(opts);
        return new Promise((resolve) => {
          pending = (res, deltaMs) => {
            perfClock += deltaMs;
            wallClock += deltaMs;
            resolve(res);
          };
        });
      },
    });

    poller.setEnabled(true);
    const p1 = poller.trigger({ force: false, reason: "poll" });
    const p2 = poller.trigger({ force: false, reason: "poll" });
    assert.equal(p1, p2);
    assert.equal(fetchCalls.length, 1);

    pending({ unchanged: true, serverMs: 12 }, 462);
    await p1;
    assert.equal(poller.getMetrics().lastNetRttMs, 450);
    assert.equal(poller.getMetrics().currentIntervalMs, POLL_INTERVAL_SLOW_MS);

    // Switching to hidden MUST keep polling at background cadence (NOT pause!)
    hidden = true;
    poller.handleVisibilityChange(true);
    assert.equal(timers.size, 1);
    const [bgTimerId, bgTimer] = [...timers.entries()][0];
    assert.equal(bgTimer.ms, BG_POLL_INTERVAL_SLOW_MS);
    assert.equal(poller.getMetrics().inBackground, true);

    // Fire the background timer while still hidden
    timers.delete(bgTimerId);
    bgTimer.fn();
    assert.equal(fetchCalls.length, 2);
    assert.deepEqual(fetchCalls[1], { force: false, reason: "poll" });
    pending({ unchanged: false, serverMs: 5 }, 35);
    await new Promise((r) => setImmediate(r));
    assert.equal(timers.size, 1);

    // Becoming visible triggers immediate forced reveal refresh
    hidden = false;
    wallClock += 500;
    poller.handleVisibilityChange(false);
    assert.equal(fetchCalls.length, 3);
    assert.deepEqual(fetchCalls[2], { force: true, reason: "visibility" });
    pending({ unchanged: false, serverMs: 5 }, 25);
    await new Promise((r) => setImmediate(r));
    assert.equal(timers.size, 1);
    poller.destroy();
  });

  it("triggers instant fresh reveal refresh with 250ms throttle across observers & events and disconnects cleanly", async () => {
    let wallClock = 10_000;
    let hidden = false;
    const calls = [];
    const poller = createAdaptivePoller({
      isHidden: () => hidden,
      now: () => wallClock,
      perfNow: () => wallClock,
      setTimer: () => 1,
      clearTimer: () => {},
      fetcher: async (opts) => {
        calls.push(opts);
        return { unchanged: true, serverMs: 2 };
      },
    });
    poller.markInitialBoot(wallClock);

    let roCb = null;
    let roDisconnected = false;
    class FakeResizeObserver {
      constructor(cb) { roCb = cb; }
      observe() {}
      disconnect() { roDisconnected = true; }
    }

    let ioCb = null;
    let ioOpts = null;
    let ioDisconnected = false;
    class FakeIntersectionObserver {
      constructor(cb, opts) { ioCb = cb; ioOpts = opts; }
      observe() {}
      disconnect() { ioDisconnected = true; }
    }

    const mkEmitter = () => {
      const map = new Map();
      return {
        addEventListener(type, fn, opts) {
          if (!map.has(type)) map.set(type, []);
          map.get(type).push({ fn, opts });
        },
        removeEventListener(type, fn) {
          map.set(type, (map.get(type) || []).filter((e) => e.fn !== fn));
        },
        emit(type) {
          for (const e of map.get(type) || []) e.fn();
        },
        listeners(type) {
          return map.get(type) || [];
        },
      };
    };

    const doc = Object.assign(mkEmitter(), { get hidden() { return hidden; } });
    const win = mkEmitter();
    let rect = { width: 0, height: 0 };
    const target = Object.assign(mkEmitter(), { getBoundingClientRect: () => rect });

    const detach = attachRevealRefresh({
      doc,
      win,
      target,
      poller,
      ResizeObserverCtor: FakeResizeObserver,
      IntersectionObserverCtor: FakeIntersectionObserver,
    });

    assert.deepEqual(ioOpts, { threshold: 0 });
    assert.equal(target.listeners("pointerenter")[0]?.opts?.passive, true);

    // 1. ResizeObserver collapsed (<=0) -> visible (>0) edge triggers force refresh
    rect = { width: 380, height: 640 };
    roCb([{ contentRect: rect }]);
    await new Promise((r) => setImmediate(r));
    assert.equal(calls.length, 1);
    assert.deepEqual(calls[0], { force: true, reason: "resize" });

    // Within 250ms throttle, rapid focus/pageshow does NOT double-fire
    wallClock += REVEAL_THROTTLE_MS - 50;
    win.emit("focus");
    win.emit("pageshow");
    await new Promise((r) => setImmediate(r));
    assert.equal(calls.length, 1);

    // After 250ms throttle, IntersectionObserver !wasVis -> isIntersecting edge triggers
    wallClock += 100;
    ioCb([{ isIntersecting: false }]);
    ioCb([{ isIntersecting: true }]);
    await new Promise((r) => setImmediate(r));
    assert.equal(calls.length, 2);
    assert.deepEqual(calls[1], { force: true, reason: "intersection" });

    // 2. Passive pointerenter skips when fresh (<= 900ms), fires when stale (> 900ms)
    wallClock += POINTER_STALE_MS - 100;
    target.emit("pointerenter");
    await new Promise((r) => setImmediate(r));
    assert.equal(calls.length, 2);

    wallClock += 200;
    target.emit("pointerenter");
    await new Promise((r) => setImmediate(r));
    assert.equal(calls.length, 3);
    assert.deepEqual(calls[2], { force: true, reason: "pointerenter" });

    // 3. detach() disconnects observers & removes listeners
    detach();
    assert.equal(roDisconnected, true);
    assert.equal(ioDisconnected, true);
    assert.equal(doc.listeners("visibilitychange").length, 0);
    assert.equal(win.listeners("focus").length, 0);
    assert.equal(win.listeners("pageshow").length, 0);
    assert.equal(target.listeners("pointerenter").length, 0);
    poller.destroy();
  });

  it("debounces 250ms drag events, coalesces single-flight last-write-wins, and rolls back on error", async () => {
    assert.equal(clampPercent(-5), 0);
    assert.equal(clampPercent(150), 100);

    const timers = new Map();
    let tid = 1;
    const sent = [];
    const committed = [];
    const rollbacks = [];
    const resolvers = [];

    const ctrl = createBrightnessController({
      debounceMs: BRIGHTNESS_DEBOUNCE_MS,
      setTimer: (fn, ms) => {
        const id = tid++;
        timers.set(id, { fn, ms });
        return id;
      },
      clearTimer: (id) => timers.delete(id),
      onCommit: (res, p) => committed.push({ res, p }),
      onRollback: (err) => rollbacks.push(err.message),
      setter: (pct) => {
        sent.push(pct);
        return new Promise((resolve, reject) => resolvers.push({ resolve, reject }));
      },
    });

    ctrl.schedule(10);
    ctrl.schedule(40);
    ctrl.schedule(70);
    assert.equal(sent.length, 0);
    const [entry] = [...timers.values()];
    timers.clear();
    entry.fn();
    assert.deepEqual(sent, [70]);

    ctrl.schedule(20, { immediate: true });
    ctrl.schedule(90, { immediate: true });
    assert.deepEqual(sent, [70]);

    resolvers.shift().resolve({ ok: true });
    await new Promise((r) => setImmediate(r));
    assert.equal(committed.length, 0);
    assert.deepEqual(sent, [70, 90]);

    resolvers.shift().resolve({ ok: true });
    await new Promise((r) => setImmediate(r));
    assert.equal(committed.length, 1);
    assert.equal(committed[0].p, 90);

    ctrl.schedule(100, { immediate: true });
    resolvers.shift().reject(new Error("boom"));
    await new Promise((r) => setImmediate(r));
    assert.deepEqual(rollbacks, ["boom"]);
    ctrl.destroy();
  });

  it("ignores transparent or low-alpha --color-background-primary host vars and returns null luminance", () => {
    const props = new Map();
    const fakeEl = {
      style: {
        setProperty: (k, v) => props.set(k, v),
      },
    };

    applyHostStyleVariables(
      {
        "--color-background-primary": "",
        "--color-text-primary": "#F4F1EA",
      },
      fakeEl
    );
    assert.equal(props.has("--color-background-primary"), false);
    assert.equal(props.get("--color-text-primary"), "#F4F1EA");

    for (const badBg of [
      "transparent",
      "TRANSPARENT",
      "rgba(0, 0, 0, 0)",
      "rgba(20, 19, 17, 0.5)",
      "rgba(255, 255, 255, 0.89)",
      "rgb(0 0 0 / 0)",
      "rgb(0 0 0 / 40%)",
      "#14131100",
      "#14131180",
      "#0000",
      "hsla(0, 0%, 0%, 0)",
      "   ",
    ]) {
      applyHostStyleVariables({ "--color-background-primary": badBg }, fakeEl);
      assert.equal(
        props.has("--color-background-primary"),
        false,
        `expected ${JSON.stringify(badBg)} to be ignored`
      );
    }

    applyHostStyleVariables({ "--color-background-primary": "rgba(20, 19, 17, 0.95)" }, fakeEl);
    assert.equal(props.get("--color-background-primary"), "rgba(20, 19, 17, 0.95)");

    applyHostStyleVariables({ "--color-background-primary": "#141311" }, fakeEl);
    assert.equal(props.get("--color-background-primary"), "#141311");

    assert.equal(parseColorLum("transparent"), null);
    assert.equal(parseColorLum("rgba(0, 0, 0, 0)"), null);
    assert.equal(parseColorLum("rgba(255, 255, 255, 0.49)"), null);
    assert.equal(parseColorLum("#ffffff00"), null);
    assert.equal(parseColorLum("#ffffff40"), null);
    assert.equal(parseColorLum("#fff0"), null);
    assert.ok(parseColorLum("#141311") < 0.2);
    assert.ok(parseColorLum("#141311ff") < 0.2);
    assert.ok(parseColorLum("#FAF8F5") > 0.9);
    assert.ok(parseColorLum("rgba(250, 248, 245, 0.9)") > 0.9);
  });

  it("built dist/panel.html is <45 KiB, CSP-safe, warm-editorial themed, zero-flicker dark-first with lock button and reveal hooks", () => {
    const html = fs.readFileSync(path.join(PANEL_DIR, "dist", "panel.html"), "utf8");
    const bytes = Buffer.byteLength(html, "utf8");
    assert.ok(bytes < 45 * 1024, `expected <45 KiB, got ${(bytes / 1024).toFixed(1)} KiB`);
    assert.ok(html.includes('<meta name="color-scheme" content="dark">'), "missing dark-only color-scheme meta in head");
    assert.ok(html.includes('style="background:#141311;color-scheme:dark"'), "missing inline dark lock on <html>");
    assert.ok(html.includes("/*__MAC_CLAMSHELL_BOOT__*/"), "missing SSR boot placeholder");
    assert.ok(html.includes("__MAC_CLAMSHELL_BOOT__"), "missing SSR boot hydration");
    assert.ok(!/https?:\/\/[^"'\s>]+/i.test(html.replace(/http:\/\/www\.w3\.org\/2000\/svg/g, "")), "must contain zero external URLs for default CSP");
    assert.ok((html.includes("#141311") || html.includes("#141311")) && (html.includes("#FAF8F5") || html.includes("#faf8f5")), "missing warm editorial surface tokens");
    assert.ok(html.includes("#D97757") || html.includes("#d97757"), "missing warm terracotta accent");
    assert.ok(!/prefers-color-scheme:\s*light/i.test(html), "must NOT override dark frame 0 via prefers-color-scheme: light");
    assert.ok(html.includes("[data-theme=light]") || html.includes('[data-theme="light"]'), "light theme must be gated on data-theme=light");
    assert.ok(html.includes("prefers-reduced-motion:reduce") || html.includes("prefers-reduced-motion: reduce"), "missing reduced-motion support");
    assert.ok(!html.includes("data-entering"), "must NOT use opacity:0 entrance animation that causes refresh flicker");
    assert.ok(/html,body\{[^}]*background-color:var\(--color-background-primary,\s*#141311\)/i.test(html), "root html,body must default to dark warm obsidian #141311 on frame 0");
    assert.ok(html.includes("containerDimensions"), "ResizeObserver must be gated on inline containerDimensions");
    assert.ok(html.includes("IntersectionObserver") && html.includes("pointerenter"), "missing reveal refresh hooks");
    const appSrc = fs.readFileSync(path.join(PANEL_DIR, "app.js"), "utf8");
    const patchAllBody = appSrc.slice(appSrc.indexOf("function patchAll("), appSrc.indexOf("function mountShell("));
    assert.ok(!patchAllBody.includes("root.innerHTML"), "patchAll must strictly patch in-place without rebuilding root.innerHTML");
    assert.ok(html.includes("diag-tray") && html.includes("bright-track") && html.includes("mode-rail") && html.includes("lock-btn"), "missing progressive disclosure tray, controls, and lock button");
    assert.ok(
      html.includes("clamshell_refresh") &&
        html.includes("clamshell_configure") &&
        html.includes("clamshell_set_brightness") &&
        html.includes("clamshell_lock_screen"),
      "missing expected MCP tool names"
    );
    for (const forbidden of ["Normal 正常", "max_jobs=", "拖动或点按轨道", "自动跟随"]) {
      assert.ok(!html.includes(forbidden), `panel.html must not contain cluttered text: ${forbidden}`);
    }
    const m = html.match(/<script>\/\*__MAC_CLAMSHELL_BOOT__\*\/<\/script><script>([\s\S]+)<\/script>/);
    assert.ok(m, "missing inline script");
    new Function(m[1]);
  });

  it("panel runtime handles background poll DOM updates, hostContext reveal refresh, lock button RPC, and teardown", async () => {
    const html = fs.readFileSync(path.join(PANEL_DIR, "dist", "panel.html"), "utf8");
    const m = html.match(/<script>\/\*__MAC_CLAMSHELL_BOOT__\*\/<\/script><script>([\s\S]+)<\/script>/);
    assert.ok(m, "missing inline script");

    const byId = new Map();
    const mkEl = (id = "", attrs = {}) => {
      const attrMap = new Map(Object.entries(attrs));
      let textVal = "";
      let innerVal = "";
      const listeners = new Map();
      const el = {
        id,
        disabled: false,
        hidden: false,
        tabIndex: 0,
        className: "",
        title: "",
        style: { setProperty() {}, backgroundColor: "", colorScheme: "" },
        lastElementChild: null,
        get textContent() { return textVal; },
        set textContent(v) { el._textWrites = (el._textWrites || 0) + 1; textVal = String(v); },
        get innerHTML() { return innerVal; },
        set innerHTML(v) { innerVal = String(v); },
        setAttribute(k, v) { attrMap.set(k, String(v)); },
        getAttribute(k) { return attrMap.has(k) ? attrMap.get(k) : null; },
        removeAttribute(k) { attrMap.delete(k); },
        addEventListener(t, fn) {
          if (!listeners.has(t)) listeners.set(t, []);
          listeners.get(t).push(fn);
        },
        removeEventListener(t, fn) {
          listeners.set(t, (listeners.get(t) || []).filter((f) => f !== fn));
        },
        dispatch(t, ev) {
          for (const fn of listeners.get(t) || []) fn(ev);
        },
        querySelector(sel) {
          const idM = sel.match(/^#(.+)$/);
          return idM ? byId.get(idM[1]) || null : null;
        },
        querySelectorAll() { return []; },
        getBoundingClientRect() { return { width: 380, height: 600, left: 0 }; },
      };
      if (id) byId.set(id, el);
      return el;
    };

    const appRoot = mkEl("app");
    const initInnerSetter = Object.getOwnPropertyDescriptor(appRoot, "innerHTML").set;
    Object.defineProperty(appRoot, "innerHTML", {
      get: () => appRoot._rawHtml || "",
      set: (v) => {
        appRoot._rawHtml = String(v);
        initInnerSetter.call(appRoot, v);
        for (const id of [
          "fb", "auto-chip", "lock-btn", "refresh-btn", "lang-btn", "t-lock", "hd", "hs", "ht", "ft",
          "bt", "bv", "biv", "bright-track", "bc", "gs", "guard-btn", "mode-rail", "mb", "sv", "ar",
          "jd", "ji", "tp", "dl", "ds", "dg", "dp", "dbb", "dbf", "dlm", "dt", "dsp", "dld", "dmu",
          "dmt", "dmf", "dc", "dhi", "de", "dh", "diag-tray",
        ]) {
          if (!byId.has(id)) {
            const e = mkEl(id);
            if (id === "auto-chip" || id === "guard-btn") e.lastElementChild = mkEl(`${id}-lbl`);
            if (id === "lock-btn") e.setAttribute("data-a", "lock");
          }
        }
      },
    });

    const docEl = mkEl("__html");
    const bodyEl = mkEl("__body");
    let docHidden = false;
    const docListeners = new Map();
    const winListeners = new Map();
    const timers = [];
    let tid = 1;
    const rpcCalls = [];
    let nextPollPayload = { unchanged: true, hash: "h1", timestamp: "2026-01-01T00:00:05Z", serverMs: 2 };
    let roDisconnected = false;
    let ioDisconnected = false;

    const fakeParent = {
      postMessage(msg) {
        if (msg.method === "ui/initialize") {
          setImmediate(() =>
            emitWin("message", {
              source: fakeParent,
              data: { jsonrpc: "2.0", id: msg.id, result: { hostCapabilities: {}, hostContext: { theme: "dark" } } },
            })
          );
        } else if (msg.method === "tools/call") {
          rpcCalls.push(msg.params);
          let structuredContent = nextPollPayload;
          if (msg.params.name === "clamshell_lock_screen") {
            structuredContent = { ok: true, method: "SACLockScreenImmediate", locked_at_ms: 1700000000000, serverMs: 1 };
          }
          setImmediate(() =>
            emitWin("message", {
              source: fakeParent,
              data: { jsonrpc: "2.0", id: msg.id, result: { content: [{ type: "text", text: "ok" }], structuredContent } },
            })
          );
        }
      },
    };

    const emitWin = (type, ev) => {
      for (const fn of winListeners.get(type) || []) fn(ev);
    };

    const boot = {
      timestamp: "2026-01-01T00:00:00Z",
      locale: "en",
      lidState: "open",
      clamshellCausesSleep: "no",
      powerSource: "ac",
      batteryPct: 95,
      thermalState: "ok",
      thermalPressure: "nominal",
      lowPowerMode: "0",
      cpuTopology: { total: 10, performanceCores: 6, efficiencyCores: 4 },
      host: { hostname: "mbp", chip: "Apple M3", macosVersion: "15.4", uptimeSeconds: 3600, loadAvg: [1, 1, 1], memory: { usedGb: 8, totalGb: 16, compressedGb: 1, usedPct: 50 } },
      settings: { modeOverride: "auto", maxJobsOverride: 0, guardEnabled: true, acClosedProfile: "utility" },
      policy: { profile: "normal", modeSource: "auto", maxJobs: 10, niceLevel: 0 },
      envVars: {},
      display: { available: true, percent: 60, savedPercent: 60, asleep: false },
      guardStatus: "armed",
      hash: "h1",
    };

    let clock = 50_000;
    const ctx = {
      window: {
        parent: fakeParent,
        innerWidth: 400,
        __MAC_CLAMSHELL_BOOT__: boot,
        addEventListener(t, fn) {
          if (!winListeners.has(t)) winListeners.set(t, []);
          winListeners.get(t).push(fn);
        },
        removeEventListener(t, fn) {
          winListeners.set(t, (winListeners.get(t) || []).filter((f) => f !== fn));
        },
      },
      document: {
        documentElement: docEl,
        body: bodyEl,
        get hidden() { return docHidden; },
        getElementById: (id) => byId.get(id) || null,
        querySelector: () => null,
        addEventListener(t, fn) {
          if (!docListeners.has(t)) docListeners.set(t, []);
          docListeners.get(t).push(fn);
        },
        removeEventListener(t, fn) {
          docListeners.set(t, (docListeners.get(t) || []).filter((f) => f !== fn));
        },
      },
      navigator: { language: "en-US" },
      performance: { now: () => clock },
      Date: Object.assign(function (...a) { return new Date(...a); }, { now: () => clock }),
      setTimeout: (fn, ms) => {
        const id = tid++;
        timers.push({ id, fn, ms, cleared: false });
        return id;
      },
      clearTimeout: (id) => {
        const t = timers.find((x) => x.id === id);
        if (t) t.cleared = true;
      },
      requestAnimationFrame: (fn) => fn(),
      ResizeObserver: class {
        observe() {}
        disconnect() { roDisconnected = true; }
      },
      IntersectionObserver: class {
        observe() {}
        disconnect() { ioDisconnected = true; }
      },
      console,
    };
    ctx.globalThis = ctx;

    vm.runInNewContext(m[1], ctx);
    await new Promise((r) => setImmediate(r));
    await new Promise((r) => setImmediate(r));

    assert.ok(byId.get("hs").textContent.includes("Open"));
    const writesAfterBoot = byId.get("hs")._textWrites;

    // 1. Hide document -> background poll with unchanged:true causes ZERO DOM writes to status
    docHidden = true;
    for (const fn of docListeners.get("visibilitychange") || []) fn();
    const bgTimer1 = [...timers].reverse().find((t) => !t.cleared && t.ms >= BG_POLL_INTERVAL_FAST_MS);
    assert.ok(bgTimer1, "expected background poll timer when hidden");
    bgTimer1.cleared = true;
    bgTimer1.fn();
    await new Promise((r) => setImmediate(r));
    await new Promise((r) => setImmediate(r));
    assert.equal(rpcCalls[rpcCalls.length - 1].name, "clamshell_refresh");
    assert.equal(rpcCalls[rpcCalls.length - 1].arguments.ifHash, "h1");
    assert.equal(rpcCalls[rpcCalls.length - 1].arguments.force, false);
    assert.equal(byId.get("hs")._textWrites, writesAfterBoot, "unchanged poll must cause 0 DOM churn");

    // 2. Background poll with CHANGED state MUST unconditionally update DOM while still hidden!
    nextPollPayload = { ...boot, lidState: "closed", batteryPct: 42, powerSource: "battery", hash: "h2", unchanged: false, serverMs: 3 };
    const bgTimer2 = [...timers].reverse().find((t) => !t.cleared && t.ms >= BG_POLL_INTERVAL_FAST_MS);
    assert.ok(bgTimer2, "expected next background poll timer");
    bgTimer2.cleared = true;
    bgTimer2.fn();
    await new Promise((r) => setImmediate(r));
    await new Promise((r) => setImmediate(r));
    assert.ok(byId.get("hs").textContent.includes("Closed"), "changed background poll must render DOM unconditionally");
    assert.ok(byId.get("hs").textContent.includes("42%"));

    // 3. onhostcontextchanged with containerDimensions triggers immediate fresh refresh
    clock += 1000;
    const prevCount = rpcCalls.length;
    emitWin("message", {
      source: fakeParent,
      data: { jsonrpc: "2.0", method: "ui/notifications/host-context-changed", params: { containerDimensions: { width: 420 } } },
    });
    await new Promise((r) => setImmediate(r));
    await new Promise((r) => setImmediate(r));
    assert.equal(rpcCalls.length, prevCount + 1);
    assert.equal(rpcCalls[rpcCalls.length - 1].name, "clamshell_refresh");
    assert.equal(rpcCalls[rpcCalls.length - 1].arguments.force, true);
    assert.equal(rpcCalls[rpcCalls.length - 1].arguments.fresh, true);

    // 4. Clicking Lock button calls clamshell_lock_screen and renders whisper-quiet feedback
    const lockBtn = byId.get("lock-btn");
    appRoot.dispatch("click", { target: { closest: () => lockBtn } });
    await new Promise((r) => setImmediate(r));
    await new Promise((r) => setImmediate(r));
    assert.equal(rpcCalls[rpcCalls.length - 1].name, "clamshell_lock_screen");
    assert.deepEqual({ ...rpcCalls[rpcCalls.length - 1].arguments }, {});
    assert.ok(byId.get("fb").innerHTML.includes("✓ Locked"));

    // 5. Teardown cleanly disconnects observers
    emitWin("message", {
      source: fakeParent,
      data: { jsonrpc: "2.0", id: 999, method: "ui/resource-teardown", params: {} },
    });
    await new Promise((r) => setImmediate(r));
    assert.equal(roDisconnected, true);
    assert.equal(ioDisconnected, true);
  });

  it("resolves bilingual panel language, guard status descriptions, and optimistic state", () => {
    assert.equal(resolvePanelLang({ saved: "zh-CN", hostLocale: "en-US" }), "zh-CN");
    assert.equal(resolvePanelLang({ saved: null, hostLocale: "zh-Hans-CN", navLang: "en" }), "zh-CN");
    assert.equal(resolvePanelLang({ saved: null, hostLocale: "fr-FR", navLang: "en-US" }), "en");

    assert.equal(
      describeGuardStatus({ settings: { guardEnabled: false } }, 80, "en"),
      "Guard paused"
    );
    assert.equal(
      describeGuardStatus({ guardStatus: "protecting", settings: { guardEnabled: true } }, 65, "zh-CN"),
      "熄屏保护中 · 恢复 65%"
    );

    const base = {
      lidState: "open",
      cpuTopology: { total: 10 },
      policy: { profile: "normal", autoProfile: "normal", maxJobs: 10 },
      guardStatus: "armed",
      settings: { modeOverride: "auto", maxJobsOverride: 0, guardEnabled: true },
      envVars: { CARGO_BUILD_JOBS: "4" },
    };
    const paused = computeOptimisticState(base, { guardEnabled: false, modeOverride: "eco", maxJobsOverride: 3 });
    assert.equal(paused.guardStatus, "paused");
    assert.equal(paused.policy.profile, "eco");
    assert.equal(paused.policy.modeSource, "manual");
    assert.equal(paused.policy.maxJobs, 3);
  });
});
