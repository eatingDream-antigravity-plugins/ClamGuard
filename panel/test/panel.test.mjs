import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  POLL_INTERVAL_FAST_MS,
  POLL_INTERVAL_MAX_BACKOFF_MS,
  POLL_INTERVAL_MED_MS,
  POLL_INTERVAL_SLOW_MS,
  POLL_INTERVAL_ULTRA_SLOW_MS,
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
  it("adapts interval from net RTT (totalMs - serverMs), enforces single-flight, and pauses when hidden", async () => {
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 40 }), POLL_INTERVAL_FAST_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 220 }), POLL_INTERVAL_MED_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 450 }), POLL_INTERVAL_SLOW_MS);
    assert.equal(computeAdaptiveIntervalMs({ rttMs: 950 }), POLL_INTERVAL_ULTRA_SLOW_MS);
    assert.equal(
      computeAdaptiveIntervalMs({ rttMs: 450, consecutiveErrors: 4 }),
      POLL_INTERVAL_MAX_BACKOFF_MS
    );

    let hidden = false;
    let wallClock = 10_000;
    let perfClock = 1000;
    let tid = 1;
    const timers = new Map();
    let fetches = 0;
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
      fetcher: () => {
        fetches += 1;
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
    assert.equal(fetches, 1);

    pending({ unchanged: true, serverMs: 12 }, 462);
    await p1;
    assert.equal(poller.getMetrics().lastNetRttMs, 450);
    assert.equal(poller.getMetrics().currentIntervalMs, POLL_INTERVAL_SLOW_MS);

    hidden = true;
    poller.handleVisibilityChange(true);
    assert.equal(timers.size, 0);
    assert.equal(await poller.trigger({ force: false, reason: "poll" }), null);

    hidden = false;
    poller.handleVisibilityChange(false);
    assert.equal(fetches, 2);
    pending({ unchanged: false, serverMs: 5 }, 35);
    await new Promise((r) => setImmediate(r));
    assert.equal(timers.size, 1);
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

  it("built dist/panel.html is <38 KiB, CSP-safe, warm-editorial themed, progressive-disclosure UX with one-shot entrance", () => {
    const html = fs.readFileSync(path.join(PANEL_DIR, "dist", "panel.html"), "utf8");
    const bytes = Buffer.byteLength(html, "utf8");
    assert.ok(bytes < 38 * 1024, `expected <38 KiB, got ${(bytes / 1024).toFixed(1)} KiB`);
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
    const appSrc = fs.readFileSync(path.join(PANEL_DIR, "app.js"), "utf8");
    const patchAllBody = appSrc.slice(appSrc.indexOf("function patchAll("), appSrc.indexOf("function mountShell("));
    assert.ok(!patchAllBody.includes("root.innerHTML"), "patchAll must strictly patch in-place without rebuilding root.innerHTML");
    assert.ok(html.includes("diag-tray") && html.includes("bright-track") && html.includes("mode-rail"), "missing progressive disclosure tray and direct manipulation controls");
    assert.ok(html.includes("clamshell_refresh") && html.includes("clamshell_configure") && html.includes("clamshell_set_brightness"), "missing expected MCP tool names");
    for (const forbidden of ["Normal 正常", "max_jobs=", "拖动或点按轨道", "自动跟随"]) {
      assert.ok(!html.includes(forbidden), `panel.html must not contain cluttered text: ${forbidden}`);
    }
    const m = html.match(/<script>\/\*__MAC_CLAMSHELL_BOOT__\*\/<\/script><script>([\s\S]+)<\/script>/);
    assert.ok(m, "missing inline script");
    new Function(m[1]);
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
