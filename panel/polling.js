// Strict single-flight RTT-adaptive polling (foreground + background) & reveal refresh.
export const POLL_INTERVAL_FAST_MS = 3_000, POLL_INTERVAL_MED_MS = 5_000, POLL_INTERVAL_SLOW_MS = 10_000, POLL_INTERVAL_ULTRA_SLOW_MS = 20_000;
export const BG_POLL_INTERVAL_FAST_MS = 6_000, BG_POLL_INTERVAL_MED_MS = 10_000, BG_POLL_INTERVAL_SLOW_MS = 18_000, BG_POLL_INTERVAL_ULTRA_SLOW_MS = 30_000;
export const POLL_INTERVAL_MAX_BACKOFF_MS = 120_000, RTT_MED_THRESHOLD_MS = 150, RTT_HIGH_THRESHOLD_MS = 350, RTT_VERY_HIGH_THRESHOLD_MS = 800;
export const REVEAL_THROTTLE_MS = 250, POINTER_STALE_MS = 900;

export function computeAdaptiveIntervalMs({ rttMs = 0, consecutiveErrors = 0, hidden = false } = {}) {
  const r = Number.isFinite(rttMs) && rttMs > 0 ? rttMs : 0;
  const idx = r > RTT_VERY_HIGH_THRESHOLD_MS ? 3 : r > RTT_HIGH_THRESHOLD_MS ? 2 : r > RTT_MED_THRESHOLD_MS ? 1 : 0;
  const base = (hidden ? [BG_POLL_INTERVAL_FAST_MS, BG_POLL_INTERVAL_MED_MS, BG_POLL_INTERVAL_SLOW_MS, BG_POLL_INTERVAL_ULTRA_SLOW_MS] : [POLL_INTERVAL_FAST_MS, POLL_INTERVAL_MED_MS, POLL_INTERVAL_SLOW_MS, POLL_INTERVAL_ULTRA_SLOW_MS])[idx];
  const errs = Math.max(0, Math.floor(Number(consecutiveErrors) || 0));
  return errs <= 0 ? base : Math.min(POLL_INTERVAL_MAX_BACKOFF_MS, base * (1 << Math.min(errs, 4)));
}

export function createAdaptivePoller({
  fetcher, defaultEnabled = true, isHidden = () => (typeof document !== "undefined" ? Boolean(document.hidden) : false),
  shouldSkipBackgroundPoll = () => false, onMetrics = () => {}, now = () => Date.now(),
  perfNow = () => (typeof performance !== "undefined" ? performance.now() : Date.now()),
  setTimer = (fn, ms) => setTimeout(fn, ms), clearTimer = (id) => clearTimeout(id), revealThrottleMs = REVEAL_THROTTLE_MS,
} = {}) {
  let enabled = !!defaultEnabled, destroyed = false, inFlight = null, queued = false, tid = null;
  let lastTotalMs = null, lastServerMs = null, lastNetRttMs = null, ewmaRttMs = null, consecutiveErrors = 0;
  let inBackground = !!isHidden(), currentIntervalMs = computeAdaptiveIntervalMs({ hidden: inBackground }), lastSuccessTs = 0, lastForceTs = -Infinity, unchangedHits = 0;

  const getMetrics = () => ({ enabled, inFlight: !!inFlight, inBackground, pausedHidden: false, lastTotalMs, lastServerMs, lastNetRttMs, ewmaRttMs, consecutiveErrors, currentIntervalMs, lastSuccessTs, unchangedHits });
  const emit = () => onMetrics(getMetrics());
  const recomp = () => { inBackground = !!isHidden(); currentIntervalMs = computeAdaptiveIntervalMs({ rttMs: ewmaRttMs ?? 0, consecutiveErrors, hidden: inBackground }); };
  const cancel = () => { if (tid !== null) { clearTimer(tid); tid = null; } };
  const sample = (tot, srv) => {
    lastTotalMs = Math.max(0, Math.round(Number(tot) || 0));
    lastServerMs = Math.max(0, Math.round(Number(srv) || 0));
    lastNetRttMs = Math.max(0, lastTotalMs - lastServerMs);
    ewmaRttMs = ewmaRttMs === null ? lastNetRttMs : Math.round(ewmaRttMs * 0.65 + lastNetRttMs * 0.35);
    recomp();
  };

  function scheduleNext() {
    cancel(); if (destroyed || !enabled) return; recomp();
    tid = setTimer(() => {
      tid = null; if (destroyed || !enabled) return emit();
      if (shouldSkipBackgroundPoll()) return scheduleNext();
      trigger({ force: false, reason: "poll" }).catch(() => {});
    }, currentIntervalMs);
  }

  function trigger({ force = false, reason = "manual" } = {}) {
    if (destroyed) return Promise.resolve(null);
    if (reason === "poll" && !enabled) { emit(); return Promise.resolve(null); }
    if (inFlight) { if (force) queued = true; return inFlight; }
    cancel(); const t0 = perfNow();
    inFlight = (async () => {
      try {
        const res = await fetcher({ force: !!force, reason });
        consecutiveErrors = 0; lastSuccessTs = now();
        if (res?.unchanged) unchangedHits += 1;
        sample(perfNow() - t0, res?.serverMs);
        return res;
      } catch (err) {
        consecutiveErrors += 1; recomp(); throw err;
      } finally {
        inFlight = null; recomp(); emit();
        if (!destroyed) {
          if (queued) { queued = false; trigger({ force: true, reason: "queued" }).catch(() => {}); }
          else if (enabled) scheduleNext();
        }
      }
    })();
    emit(); return inFlight;
  }

  const forceNow = (reason = "reveal") => {
    if (destroyed) return Promise.resolve(null);
    const t = now(); if (t - lastForceTs < revealThrottleMs) return inFlight || Promise.resolve(null);
    lastForceTs = t; return trigger({ force: true, reason });
  };
  return {
    trigger, forceNow, getMetrics,
    isStale: (maxAgeMs = POINTER_STALE_MS) => lastSuccessTs === 0 || now() - lastSuccessTs > maxAgeMs,
    setEnabled(next) { enabled = !!next; cancel(); recomp(); if (enabled && !inFlight) scheduleNext(); emit(); return enabled; },
    handleVisibilityChange(ov) {
      if (destroyed) return;
      const hidden = ov !== undefined ? !!ov : !!isHidden(), was = inBackground;
      inBackground = hidden; currentIntervalMs = computeAdaptiveIntervalMs({ rttMs: ewmaRttMs ?? 0, consecutiveErrors, hidden });
      if (hidden) { if (enabled && !inFlight) scheduleNext(); return emit(); }
      if (was) forceNow("visibility").catch(() => {}); else { if (enabled && !inFlight) scheduleNext(); emit(); }
    },
    recordExternalRpc({ totalMs, serverMs, markSuccess = true } = {}) {
      if (destroyed) return;
      if (markSuccess) { consecutiveErrors = 0; lastSuccessTs = now(); }
      sample(totalMs, serverMs); emit();
    },
    markInitialBoot(ts = now()) { if (lastSuccessTs === 0) lastSuccessTs = ts; recomp(); if (enabled && !inFlight && tid === null) scheduleNext(); emit(); },
    destroy() { destroyed = true; enabled = queued = false; cancel(); },
  };
}

export function attachRevealRefresh({
  doc = typeof document !== "undefined" ? document : null, win = typeof window !== "undefined" ? window : null,
  target = doc?.documentElement || null, poller, onReveal = (r) => poller?.forceNow?.(r)?.catch?.(() => {}),
  ResizeObserverCtor = typeof ResizeObserver !== "undefined" ? ResizeObserver : null,
  IntersectionObserverCtor = typeof IntersectionObserver !== "undefined" ? IntersectionObserver : null, stalePointerMs = POINTER_STALE_MS,
} = {}) {
  const offs = [], add = (obj, ev, fn, opts) => { if (!obj?.addEventListener) return; obj.addEventListener(ev, fn, opts); offs.push(() => obj.removeEventListener(ev, fn)); };
  if (ResizeObserverCtor && target) {
    const r0 = target.getBoundingClientRect?.(); let col = r0 ? r0.width <= 0 || r0.height <= 0 : false;
    const ro = new ResizeObserverCtor((es) => { const rc = es?.[es.length - 1]?.contentRect || target.getBoundingClientRect?.(); if (!rc) return; const c = rc.width <= 0 || rc.height <= 0; if (col && !c) onReveal?.("resize"); col = c; });
    ro.observe(target); offs.push(() => ro.disconnect());
  }
  if (IntersectionObserverCtor && target) {
    let vis = typeof doc?.hidden === "boolean" ? !doc.hidden : false;
    const io = new IntersectionObserverCtor((es) => { const e = es?.[es.length - 1]; if (!e) return; const v = !!e.isIntersecting; if (!vis && v) onReveal?.("intersection"); vis = v; }, { threshold: 0 });
    io.observe(target); offs.push(() => io.disconnect());
  }
  add(doc, "visibilitychange", () => poller?.handleVisibilityChange?.(doc.hidden));
  add(win, "focus", () => onReveal?.("focus"));
  add(win, "pageshow", () => onReveal?.("pageshow"));
  add(target, "pointerenter", () => { if (!poller?.isStale || poller.isStale(stalePointerMs)) onReveal?.("pointerenter"); }, { passive: true });
  return () => { for (const fn of offs) try { fn(); } catch {} offs.length = 0; };
}
