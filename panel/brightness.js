// Debounced, single-flight, last-write-wins brightness controller for high-RTT links.

export const BRIGHTNESS_DEBOUNCE_MS = 250;

export function clampPercent(val) {
  const n = Number(val);
  if (!Number.isFinite(n)) return 0;
  return Math.max(0, Math.min(100, Math.round(n)));
}

export function createBrightnessController({
  setter,
  debounceMs = BRIGHTNESS_DEBOUNCE_MS,
  setTimer = (fn, ms) => setTimeout(fn, ms),
  clearTimer = (id) => clearTimeout(id),
  onOptimistic = () => {},
  onCommit = () => {},
  onRollback = () => {},
  onSyncChange = () => {},
} = {}) {
  let destroyed = false;
  let inFlight = false;
  let seq = 0;
  let pendingPercent = null;
  let debounceTimer = null;

  function clearDebounce() {
    if (debounceTimer !== null) {
      clearTimer(debounceTimer);
      debounceTimer = null;
    }
  }

  async function flush() {
    clearDebounce();
    if (destroyed || inFlight || pendingPercent === null) return;
    inFlight = true;
    onSyncChange(true);

    try {
      while (!destroyed && pendingPercent !== null) {
        const target = pendingPercent;
        const mySeq = seq;
        pendingPercent = null;
        try {
          const res = await setter(target);
          if (!destroyed && mySeq === seq && pendingPercent === null) {
            onCommit(res, target);
          }
        } catch (err) {
          if (!destroyed && mySeq === seq && pendingPercent === null) {
            onRollback(err, target);
          }
          break;
        }
      }
    } finally {
      inFlight = false;
      if (!destroyed) onSyncChange(pendingPercent !== null);
    }
  }

  function schedule(percent, { immediate = false } = {}) {
    if (destroyed) return;
    const target = clampPercent(percent);
    seq += 1;
    pendingPercent = target;
    onOptimistic(target);
    onSyncChange(true);

    if (immediate) {
      flush();
      return;
    }

    clearDebounce();
    debounceTimer = setTimer(() => {
      debounceTimer = null;
      flush();
    }, debounceMs);
  }

  function isSyncing() {
    return inFlight || pendingPercent !== null || debounceTimer !== null;
  }

  function destroy() {
    destroyed = true;
    pendingPercent = null;
    clearDebounce();
  }

  return { schedule, flush, isSyncing, destroy };
}
