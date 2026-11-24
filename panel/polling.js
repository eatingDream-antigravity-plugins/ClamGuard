// Strict single-flight RTT-adaptive polling (foreground + background) & reveal refresh.

export const POLL_INTERVAL_FAST_MS = 3_000;
export const POLL_INTERVAL_MED_MS = 5_000;
export const POLL_INTERVAL_SLOW_MS = 10_000;
export const POLL_INTERVAL_ULTRA_SLOW_MS = 20_000;

export const BG_POLL_INTERVAL_FAST_MS = 6_000;
export const BG_POLL_INTERVAL_MED_MS = 10_000;
export const BG_POLL_INTERVAL_SLOW_MS = 18_000;
export const BG_POLL_INTERVAL_ULTRA_SLOW_MS = 30_000;

export const POLL_INTERVAL_MAX_BACKOFF_MS = 120_000;

export const RTT_MED_THRESHOLD_MS = 150;
export const RTT_HIGH_THRESHOLD_MS = 350;
export const RTT_VERY_HIGH_THRESHOLD_MS = 800;
export const REVEAL_THROTTLE_MS = 250;
export const POINTER_STALE_MS = 900;

export function computeAdaptiveIntervalMs({
  rttMs = 0,
  consecutiveErrors = 0,
  hidden = false,
} = {}) {
  const rtt = Number.isFinite(rttMs) && rttMs > 0 ? rttMs : 0;
  let base = hidden ? BG_POLL_INTERVAL_FAST_MS : POLL_INTERVAL_FAST_MS;
  if (rtt > RTT_VERY_HIGH_THRESHOLD_MS) {
    base = hidden ? BG_POLL_INTERVAL_ULTRA_SLOW_MS : POLL_INTERVAL_ULTRA_SLOW_MS;
  } else if (rtt > RTT_HIGH_THRESHOLD_MS) {
    base = hidden ? BG_POLL_INTERVAL_SLOW_MS : POLL_INTERVAL_SLOW_MS;
  } else if (rtt > RTT_MED_THRESHOLD_MS) {
    base = hidden ? BG_POLL_INTERVAL_MED_MS : POLL_INTERVAL_MED_MS;
  }

  const errs = Math.max(0, Math.floor(Number(consecutiveErrors) || 0));
  if (errs <= 0) return base;
  return Math.min(POLL_INTERVAL_MAX_BACKOFF_MS, base * Math.pow(2, Math.min(errs, 4)));
}

export function createAdaptivePoller({
  fetcher,
  defaultEnabled = true,
  isHidden = () => (typeof document !== "undefined" ? Boolean(document.hidden) : false),
  shouldSkipBackgroundPoll = () => false,
  onMetrics = () => {},
  now = () => Date.now(),
  perfNow = () => (typeof performance !== "undefined" ? performance.now() : Date.now()),
  setTimer = (fn, ms) => setTimeout(fn, ms),
  clearTimer = (id) => clearTimeout(id),
  revealThrottleMs = REVEAL_THROTTLE_MS,
} = {}) {
  let enabled = Boolean(defaultEnabled);
  let destroyed = false;
  let inFlight = null;
  let queuedUrgent = false;
  let timerId = null;

  let lastTotalMs = null;
  let lastServerMs = null;
  let lastNetRttMs = null;
  let ewmaRttMs = null;
  let consecutiveErrors = 0;
  let inBackground = Boolean(isHidden());
  let currentIntervalMs = computeAdaptiveIntervalMs({ rttMs: 0, consecutiveErrors: 0, hidden: inBackground });
  let lastSuccessTs = 0;
  let lastForceTs = -Infinity;
  let unchangedHits = 0;

  const getMetrics = () => ({
    enabled,
    inFlight: Boolean(inFlight),
    inBackground,
    pausedHidden: false,
    lastTotalMs,
    lastServerMs,
    lastNetRttMs,
    ewmaRttMs,
    consecutiveErrors,
    currentIntervalMs,
    lastSuccessTs,
    unchangedHits,
  });

  const emitMetrics = () => onMetrics(getMetrics());
  const recomputeInterval = () => {
    inBackground = Boolean(isHidden());
    currentIntervalMs = computeAdaptiveIntervalMs({
      rttMs: ewmaRttMs ?? 0,
      consecutiveErrors,
      hidden: inBackground,
    });
  };

  function cancelScheduled() {
    if (timerId !== null) {
      clearTimer(timerId);
      timerId = null;
    }
  }

  function updateRttSample(totalMs, serverMs) {
    const cleanTotal = Math.max(0, Math.round(Number(totalMs) || 0));
    const cleanServer = Math.max(0, Math.round(Number(serverMs) || 0));
    const netRtt = Math.max(0, cleanTotal - cleanServer);
    lastTotalMs = cleanTotal;
    lastServerMs = cleanServer;
    lastNetRttMs = netRtt;
    ewmaRttMs = ewmaRttMs === null ? netRtt : Math.round(ewmaRttMs * 0.65 + netRtt * 0.35);
    recomputeInterval();
  }

  function scheduleNext() {
    cancelScheduled();
    if (destroyed || !enabled) return;
    recomputeInterval();
    timerId = setTimer(() => {
      timerId = null;
      if (destroyed || !enabled) {
        emitMetrics();
        return;
      }
      if (shouldSkipBackgroundPoll()) {
        scheduleNext();
        return;
      }
      trigger({ force: false, reason: "poll" }).catch(() => {});
    }, currentIntervalMs);
  }

  function trigger({ force = false, reason = "manual" } = {}) {
    if (destroyed) return Promise.resolve(null);
    if (reason === "poll" && !enabled) {
      emitMetrics();
      return Promise.resolve(null);
    }

    if (inFlight) {
      if (force) queuedUrgent = true;
      return inFlight;
    }

    cancelScheduled();
    const t0 = perfNow();

    const run = (async () => {
      try {
        const res = await fetcher({ force: Boolean(force), reason });
        const totalMs = Math.max(0, Math.round(perfNow() - t0));
        const serverMs = Math.max(0, Math.round(Number(res?.serverMs) || 0));
        consecutiveErrors = 0;
        lastSuccessTs = now();
        if (res?.unchanged) unchangedHits += 1;
        updateRttSample(totalMs, serverMs);
        return res;
      } catch (err) {
        consecutiveErrors += 1;
        recomputeInterval();
        throw err;
      } finally {
        inFlight = null;
        recomputeInterval();
        emitMetrics();
        if (!destroyed) {
          if (queuedUrgent) {
            queuedUrgent = false;
            trigger({ force: true, reason: "queued" }).catch(() => {});
          } else if (enabled) {
            scheduleNext();
          }
        }
      }
    })();

    inFlight = run;
    emitMetrics();
    return run;
  }

  function forceNow(reason = "reveal") {
    if (destroyed) return Promise.resolve(null);
    const t = now();
    if (t - lastForceTs < revealThrottleMs) {
      return inFlight || Promise.resolve(null);
    }
    lastForceTs = t;
    return trigger({ force: true, reason });
  }

  function isStale(maxAgeMs = POINTER_STALE_MS) {
    return lastSuccessTs === 0 || now() - lastSuccessTs > maxAgeMs;
  }

  function setEnabled(next) {
    enabled = Boolean(next);
    cancelScheduled();
    recomputeInterval();
    if (enabled && !inFlight) scheduleNext();
    emitMetrics();
    return enabled;
  }

  function handleVisibilityChange(hiddenOverride) {
    if (destroyed) return;
    const hidden = hiddenOverride !== undefined ? Boolean(hiddenOverride) : Boolean(isHidden());
    const wasHidden = inBackground;
    inBackground = hidden;
    currentIntervalMs = computeAdaptiveIntervalMs({
      rttMs: ewmaRttMs ?? 0,
      consecutiveErrors,
      hidden,
    });

    if (hidden) {
      if (enabled && !inFlight) scheduleNext();
      emitMetrics();
      return;
    }

    if (wasHidden) {
      forceNow("visibility").catch(() => {});
    } else if (enabled && !inFlight) {
      scheduleNext();
      emitMetrics();
    } else {
      emitMetrics();
    }
  }

  function recordExternalRpc({ totalMs, serverMs, markSuccess = true } = {}) {
    if (destroyed) return;
    if (markSuccess) {
      consecutiveErrors = 0;
      lastSuccessTs = now();
    }
    updateRttSample(totalMs, serverMs);
    emitMetrics();
  }

  function markInitialBoot(ts = now()) {
    if (lastSuccessTs === 0) lastSuccessTs = ts;
    recomputeInterval();
    if (enabled && !inFlight && timerId === null) scheduleNext();
    emitMetrics();
  }

  function destroy() {
    destroyed = true;
    enabled = false;
    queuedUrgent = false;
    cancelScheduled();
  }

  return {
    trigger,
    forceNow,
    isStale,
    setEnabled,
    handleVisibilityChange,
    recordExternalRpc,
    markInitialBoot,
    getMetrics,
    destroy,
  };
}

export function attachRevealRefresh({
  doc = typeof document !== "undefined" ? document : null,
  win = typeof window !== "undefined" ? window : null,
  target = doc?.documentElement || null,
  poller,
  onReveal = (reason) => poller?.forceNow?.(reason)?.catch?.(() => {}),
  ResizeObserverCtor = typeof ResizeObserver !== "undefined" ? ResizeObserver : null,
  IntersectionObserverCtor = typeof IntersectionObserver !== "undefined" ? IntersectionObserver : null,
  stalePointerMs = POINTER_STALE_MS,
} = {}) {
  const cleanups = [];
  const reveal = (reason) => onReveal?.(reason);

  if (ResizeObserverCtor && target) {
    const r0 = target.getBoundingClientRect?.();
    let wasCollapsed = r0 ? r0.width <= 0 || r0.height <= 0 : false;
    const ro = new ResizeObserverCtor((entries) => {
      const rect = entries?.[entries.length - 1]?.contentRect || target.getBoundingClientRect?.();
      if (!rect) return;
      const collapsed = rect.width <= 0 || rect.height <= 0;
      if (wasCollapsed && !collapsed) reveal("resize");
      wasCollapsed = collapsed;
    });
    ro.observe(target);
    cleanups.push(() => ro.disconnect());
  }

  if (IntersectionObserverCtor && target) {
    let wasVis = typeof doc?.hidden === "boolean" ? !doc.hidden : false;
    const io = new IntersectionObserverCtor(
      (entries) => {
        const entry = entries?.[entries.length - 1];
        if (!entry) return;
        const vis = Boolean(entry.isIntersecting);
        if (!wasVis && vis) reveal("intersection");
        wasVis = vis;
      },
      { threshold: 0 }
    );
    io.observe(target);
    cleanups.push(() => io.disconnect());
  }

  if (doc?.addEventListener) {
    const onVis = () => poller?.handleVisibilityChange?.(doc.hidden);
    doc.addEventListener("visibilitychange", onVis);
    cleanups.push(() => doc.removeEventListener("visibilitychange", onVis));
  }

  if (win?.addEventListener) {
    const onFocus = () => reveal("focus");
    const onPageShow = () => reveal("pageshow");
    win.addEventListener("focus", onFocus);
    win.addEventListener("pageshow", onPageShow);
    cleanups.push(() => {
      win.removeEventListener("focus", onFocus);
      win.removeEventListener("pageshow", onPageShow);
    });
  }

  if (target?.addEventListener) {
    const onEnter = () => {
      if (!poller?.isStale || poller.isStale(stalePointerMs)) reveal("pointerenter");
    };
    target.addEventListener("pointerenter", onEnter, { passive: true });
    cleanups.push(() => target.removeEventListener("pointerenter", onEnter));
  }

  return () => {
    for (const fn of cleanups) {
      try {
        fn();
      } catch {}
    }
    cleanups.length = 0;
  };
}
