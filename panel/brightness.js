// Debounced, single-flight, last-write-wins brightness controller for high-RTT links.
export const BRIGHTNESS_DEBOUNCE_MS = 250;
export const clampPercent = (v) => (Number.isFinite(Number(v)) ? Math.max(0, Math.min(100, Math.round(Number(v)))) : 0);

export function createBrightnessController({
  setter, debounceMs = BRIGHTNESS_DEBOUNCE_MS, setTimer = (f, m) => setTimeout(f, m), clearTimer = (i) => clearTimeout(i),
  onOptimistic = () => {}, onCommit = () => {}, onRollback = () => {}, onSyncChange = () => {},
} = {}) {
  let dead = false, busy = false, seq = 0, pend = null, tid = null;
  const clr = () => { if (tid !== null) { clearTimer(tid); tid = null; } };
  async function flush() {
    clr(); if (dead || busy || pend === null) return;
    busy = true; onSyncChange(true);
    try {
      while (!dead && pend !== null) {
        const t = pend, s = seq; pend = null;
        try { const r = await setter(t); if (!dead && s === seq && pend === null) onCommit(r, t); }
        catch (e) { if (!dead && s === seq && pend === null) onRollback(e, t); break; }
      }
    } finally { busy = false; if (!dead) onSyncChange(pend !== null); }
  }
  return {
    schedule(pct, { immediate = false } = {}) {
      if (dead) return;
      const t = clampPercent(pct); seq += 1; pend = t; onOptimistic(t); onSyncChange(true);
      if (immediate) return flush();
      clr(); tid = setTimer(() => { tid = null; flush(); }, debounceMs);
    },
    flush,
    isSyncing: () => busy || pend !== null || tid !== null,
    destroy() { dead = true; pend = null; clr(); },
  };
}
