// Debounced, single-flight, last-write-wins brightness controller for high-RTT links.
export const BRIGHTNESS_DEBOUNCE_MS = 250;

export const clampPercent = (v: unknown): number =>
  Number.isFinite(Number(v)) ? Math.max(0, Math.min(100, Math.round(Number(v)))) : 0;

export interface BrightnessControllerOptions<TRes = unknown, TTimer = ReturnType<typeof setTimeout>> {
  setter: (percent: number) => Promise<TRes>;
  debounceMs?: number;
  setTimer?: (fn: () => void, ms: number) => TTimer;
  clearTimer?: (id: TTimer) => void;
  onOptimistic?: (percent: number) => void;
  onCommit?: (res: TRes, target: number) => void;
  onRollback?: (err: unknown, target: number) => void;
  onSyncChange?: (syncing: boolean) => void;
}

export interface BrightnessController {
  schedule(pct: number, opts?: { immediate?: boolean }): Promise<void> | undefined;
  flush(): Promise<void>;
  isSyncing(): boolean;
  destroy(): void;
}

export function createBrightnessController<TRes = unknown, TTimer = ReturnType<typeof setTimeout>>({
  setter,
  debounceMs = BRIGHTNESS_DEBOUNCE_MS,
  setTimer = (f, m) => setTimeout(f, m) as unknown as TTimer,
  clearTimer = (i) => clearTimeout(i as unknown as ReturnType<typeof setTimeout>),
  onOptimistic = () => {},
  onCommit = () => {},
  onRollback = () => {},
  onSyncChange = () => {},
}: BrightnessControllerOptions<TRes, TTimer>): BrightnessController {
  let dead = false;
  let busy = false;
  let seq = 0;
  let pend: number | null = null;
  let tid: TTimer | null = null;

  const clr = (): void => {
    if (tid !== null) {
      clearTimer(tid);
      tid = null;
    }
  };

  async function flush(): Promise<void> {
    clr();
    if (dead || busy || pend === null) return;
    busy = true;
    onSyncChange(true);
    try {
      while (!dead && pend !== null) {
        clr();
        const t = pend;
        const s = seq;
        pend = null;
        try {
          const r = await setter(t);
          if (!dead && s === seq && pend === null) onCommit(r, t);
        } catch (e) {
          if (dead) break;
          if (pend !== null) continue;
          if (s === seq) onRollback(e, t);
          break;
        }
      }
    } finally {
      busy = false;
      if (!dead) onSyncChange(pend !== null);
    }
  }

  return {
    schedule(pct: number, { immediate = false }: { immediate?: boolean } = {}): Promise<void> | undefined {
      if (dead) return;
      const t = clampPercent(pct);
      seq += 1;
      pend = t;
      onOptimistic(t);
      onSyncChange(true);
      if (immediate) return flush();
      clr();
      tid = setTimer(() => {
        tid = null;
        void flush();
      }, debounceMs);
      return;
    },
    flush,
    isSyncing: () => busy || pend !== null || tid !== null,
    destroy(): void {
      dead = true;
      pend = null;
      clr();
    },
  };
}
