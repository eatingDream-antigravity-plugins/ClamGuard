// Pure locale, policy, topology, and display-guard state helpers for Mac Clamshell panel.

export function normalizeLang(raw) {
  const s = String(raw ?? "").trim().toLowerCase();
  if (!s || s === "auto") return null;
  return s === "zh" || s.startsWith("zh-") || s.startsWith("zh_") ? "zh-CN" : "en";
}

export function resolvePanelLang({ saved, hostLocale, navLang, bootLocale } = {}) {
  for (const src of [saved, hostLocale, navLang, bootLocale]) {
    const norm = normalizeLang(src);
    if (norm) return norm;
  }
  return "en";
}

export function computeOptimisticState(base, patch) {
  if (!base) return base;
  const s = { ...base.settings, ...patch };
  const total = Math.max(1, Number(base.cpuTopology?.total) || 8);
  const basePol = base.policy || {};
  const thermalThrottled = ["warn", "serious", "critical"].includes(base.thermalState);
  let autoProfile = basePol.autoProfile || "normal";
  if (
    patch?.acClosedProfile &&
    !thermalThrottled &&
    base.lidState === "closed" &&
    base.powerSource !== "battery"
  ) {
    autoProfile = s.acClosedProfile === "eco" ? "eco" : "utility";
  }
  const modeOv = s.modeOverride || "auto";
  const manual = modeOv !== "auto";
  const profile = manual ? modeOv : autoProfile;
  const pj = base.profileJobs;
  const tableJobs = pj
    ? !manual && profile === "eco" && pj.autoEco != null
      ? pj.autoEco
      : pj[profile]
    : null;
  const maxJobs =
    s.maxJobsOverride > 0
      ? Math.min(s.maxJobsOverride, total)
      : tableJobs != null
      ? tableJobs
      : profile === "strict"
      ? 1
      : profile === "normal"
      ? total
      : basePol.maxJobs || total;

  let guardStatus = base.guardStatus || "armed";
  if (s.guardEnabled === false) guardStatus = "paused";
  else if (guardStatus === "paused") {
    guardStatus = base.lidState === "closed" ? "protecting" : "armed";
  }

  return {
    ...base,
    settings: s,
    guardStatus,
    policy: {
      ...basePol,
      profile,
      autoProfile,
      modeSource: manual ? "manual" : "auto",
      maxJobs,
    },
    envVars: profile === "normal" ? {} : base.envVars,
  };
}

export function describeGuardStatus(d, restorePct, lang = "en") {
  const zh = lang === "zh-CN";
  const enabled = d?.settings?.guardEnabled !== false;
  const st = !enabled ? "paused" : d?.guardStatus || (d?.displayGuard ? "armed" : "idle");

  if (st === "paused") return zh ? "守护已暂停" : "Guard paused";
  if (st === "protecting") {
    return zh ? `熄屏保护中 · 恢复 ${restorePct}%` : `Protecting · restores ${restorePct}%`;
  }
  if (st === "thermal_cap") {
    const pre =
      d?.displayGuard?.preThermalBrightness != null
        ? Math.round(d.displayGuard.preThermalBrightness * 100)
        : restorePct;
    return zh ? `温控限亮中 · 恢复 ${pre}%` : `Thermal cap · restores ${pre}%`;
  }
  if (st === "armed") return zh ? "守护运行中" : "Guard active";
  return zh ? "守护待命" : "Guard idle";
}
