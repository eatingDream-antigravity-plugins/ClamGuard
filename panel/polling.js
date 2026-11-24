// Strict single-flight adaptive polling controller for high-latency links.

export const POLL_INTERVAL_FAST_MS = 15_000;
export const POLL_INTERVAL_MED_MS = 22_000;
export const POLL_INTERVAL_SLOW_MS = 35_000;
export const POLL_INTERVAL_ULTRA_SLOW_MS = 60_000;
export const POLL_INTERVAL_MAX_BACKOFF_MS = 120_000;

export const RTT_MED_THRESHOLD_MS = 150;
export const RTT_HIGH_THRESHOLD_MS = 350;
export const RTT_VERY_HIGH_THRESHOLD_MS = 800;
export const STALE_ON_VISIBLE_MS = 30_000;

export function computeAdaptiveIntervalMs({ rttMs = 0, consecutiveErrors = 0 } = {}) {
  const rtt = Number.isFinite(rttMs) && rttMs > 0 ? rttMs : 0;
  let base = POLL_INTERVAL_FAST_MS;
  if (rtt > RTT_VERY_HIGH_THRESHOLD_MS) base = POLL_INTERVAL_ULTRA_SLOW_MS;
  else if (rtt > RTT_HIGH_THRESHOLD_MS) base = POLL_INTERVAL_SLOW_MS;
  else if (rtt > RTT_MED_THRESHOLD_MS) base = POLL_INTERVAL_MED_MS;

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
  staleVisibleMs = STALE_ON_VISIBLE_MS,
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
  let currentIntervalMs = POLL_INTERVAL_FAST_MS;
  let lastSuccessTs = 0;
  let unchangedHits = 0;
  let pausedHidden = Boolean(isHidden());

  const getMetrics = () => ({
    enabled,
    inFlight: Boolean(inFlight),
    pausedHidden,
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
  const syncHidden = () => {
    pausedHidden = Boolean(isHidden());
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
    currentIntervalMs = computeAdaptiveIntervalMs({ rttMs: ewmaRttMs, consecutiveErrors });
  }

  function scheduleNext() {
    cancelScheduled();
    if (destroyed || !enabled || isHidden()) {
      syncHidden();
      return;
    }
    pausedHidden = false;
    timerId = setTimer(() => {
      timerId = null;
      if (destroyed || !enabled || isHidden()) {
        syncHidden();
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
    if (reason === "poll" && (!enabled || isHidden())) {
      syncHidden();
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
        currentIntervalMs = computeAdaptiveIntervalMs({
          rttMs: ewmaRttMs ?? 0,
          consecutiveErrors,
        });
        throw err;
      } finally {
        inFlight = null;
        emitMetrics();
        if (!destroyed) {
          if (queuedUrgent && !isHidden()) {
            queuedUrgent = false;
            trigger({ force: true, reason: "queued" }).catch(() => {});
          } else if (enabled && !isHidden()) {
            scheduleNext();
          }
        }
      }
    })();

    inFlight = run;
    emitMetrics();
    return run;
  }

  function setEnabled(next) {
    enabled = Boolean(next);
    cancelScheduled();
    pausedHidden = Boolean(isHidden());
    if (enabled && !pausedHidden && !inFlight) scheduleNext();
    emitMetrics();
    return enabled;
  }

  function handleVisibilityChange(hiddenOverride) {
    if (destroyed) return;
    const hidden = hiddenOverride !== undefined ? Boolean(hiddenOverride) : Boolean(isHidden());
    const wasHidden = pausedHidden;
    pausedHidden = hidden;

    if (hidden) {
      cancelScheduled();
      emitMetrics();
      return;
    }

    const isStale = lastSuccessTs > 0 && now() - lastSuccessTs >= staleVisibleMs;
    if (wasHidden && (enabled || isStale)) {
      trigger({ force: false, reason: "visibility" }).catch(() => {});
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
    pausedHidden = Boolean(isHidden());
    if (enabled && !pausedHidden && !inFlight && timerId === null) scheduleNext();
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
    setEnabled,
    handleVisibilityChange,
    recordExternalRpc,
    markInitialBoot,
    getMetrics,
    destroy,
  };
}
