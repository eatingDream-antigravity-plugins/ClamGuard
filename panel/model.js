// Pure locale, policy, topology, display-guard, and read-only fan ownership helpers.
const SYS = "System (macOS)", MAN = "Manual (SMC)", UNK = "Unknown";

export function normalizeLang(raw) {
  const s = String(raw ?? "").trim().toLowerCase();
  if (!s || s === "auto") return null;
  return s === "zh" || s.startsWith("zh-") || s.startsWith("zh_") ? "zh-CN" : "en";
}

export function resolvePanelLang({ saved, hostLocale, navLang, bootLocale } = {}) {
  for (const src of [saved, hostLocale, navLang, bootLocale]) { const n = normalizeLang(src); if (n) return n; }
  return "en";
}

export function computeOptimisticState(base, patch) {
  if (!base) return base;
  const s = { ...base.settings, ...patch }, total = Math.max(1, +base.cpuTopology?.total || 8), bp = base.policy || {};
  let autoProfile = bp.autoProfile || "normal";
  if (patch?.acClosedProfile && !["warn", "serious", "critical"].includes(base.thermalState) && base.lidState === "closed" && base.powerSource !== "battery") {
    autoProfile = s.acClosedProfile === "eco" ? "eco" : "utility";
  }
  const ov = s.modeOverride || "auto", manual = ov !== "auto", profile = manual ? ov : autoProfile, pj = base.profileJobs;
  const tj = pj ? (!manual && profile === "eco" && pj.autoEco != null ? pj.autoEco : pj[profile]) : null;
  const maxJobs = s.maxJobsOverride > 0 ? Math.min(s.maxJobsOverride, total) : tj ?? (profile === "strict" ? 1 : profile === "normal" ? total : bp.maxJobs || total);
  let guardStatus = base.guardStatus || "armed";
  if (s.guardEnabled === false) guardStatus = "paused";
  else if (guardStatus === "paused") guardStatus = base.lidState === "closed" ? "protecting" : "armed";
  return { ...base, settings: s, guardStatus, policy: { ...bp, profile, autoProfile, modeSource: manual ? "manual" : "auto", maxJobs }, envVars: profile === "normal" ? {} : base.envVars };
}

export function describeGuardStatus(d, restorePct, lang = "en") {
  const b = (e, z) => (lang === "zh-CN" ? z : e);
  const st = d?.settings?.guardEnabled === false ? "paused" : d?.guardStatus || (d?.displayGuard ? "armed" : "idle");
  if (st === "paused") return b("Guard paused", "守护已暂停");
  if (st === "protecting") return b(`Protecting · restores ${restorePct}%`, `熄屏保护中 · 恢复 ${restorePct}%`);
  if (st === "thermal_cap") {
    const p = d?.displayGuard?.preThermalBrightness != null ? Math.round(d.displayGuard.preThermalBrightness * 100) : restorePct;
    return b(`Thermal cap · restores ${p}%`, `温控限亮中 · 恢复 ${p}%`);
  }
  return st === "armed" ? b("Guard active", "守护运行中") : b("Guard idle", "守护待命");
}

const rpm = (n) => Math.max(0, Math.round(+n || 0)).toLocaleString("en-US");

export function describeFanState(fan, lang = "en", guardPaused = false) {
  const b = (e, z) => (lang === "zh-CN" ? z : e), st = fan?.status || "unavailable", fs = Array.isArray(fan?.fans) ? fan.fans : [];
  const ex = Array.isArray(fan?.externalManagers) ? fan.externalManagers : [], mon = Array.isArray(fan?.detectedMonitors) ? fan.detectedMonitors : [];
  const y = fan?.ownershipMode === "yield", sm = fan?.smcMode || (st === "unavailable" ? "unknown" : st === "fanless" ? "none" : "auto");
  const rc = fan?.controller || (sm === "unknown" ? UNK : SYS);
  const base = rc === SYS ? b(SYS, "系统自动 (macOS)") : rc === MAN ? b(MAN, "手动策略 (SMC)") : rc === UNK ? b("Unknown (SMC)", "模式未知 (SMC)") : rc;
  const controller = ex.length && (rc === SYS || rc === UNK) ? `${base} · ${ex.join(", ")}` : base;
  const gTail = guardPaused ? b("guard paused.", "守护已暂停。") : b("guard active.", "守护运行中。");
  if (st === "fanless") {
    const p = b("Passive cooling", "无风扇被动散热");
    return { status: st, rpmText: b("No fans (passive cooling)", "无风扇（被动散热）"), showController: false, controller, badgeText: b("Observer", "只读监测"), badgeKind: "in", tooltip: p, caption: `${p}${b(" — ", "；")}${gTail}` };
  }
  const rpmText = st === "ok" && fs.length === 2 ? b(`L ${rpm(fs[0].rpm)} · R ${rpm(fs[1].rpm)} rpm`, `左 ${rpm(fs[0].rpm)} · 右 ${rpm(fs[1].rpm)} rpm`)
    : st === "ok" && fs.length ? `${fs.map((f) => rpm(f.rpm)).join(" · ")} rpm` : b("Unavailable", "传感器不可读");
  const ro = b("Read-only", "只读"), sep = b(", ", "、"), dot = b(".", "");
  const smL = sm === "manual" ? b("manual", "手动") : sm === "auto" ? b("auto", "自动") : b("unknown", "未知");
  const caption = st === "unavailable" ? b("Sensor unavailable; ", "风扇不可读；") + gTail
    : ex.length ? `${ro} · SMC ${smL} · ${ex.join(sep)}${dot}`
    : sm === "manual" ? `${ro} · SMC ${smL}${dot}`
    : mon.length ? `${ro} · SMC ${smL} · ${b("monitor", "监测")}: ${mon.join(sep)}${dot}`
    : sm === "unknown" ? `${ro} · SMC ${smL}${dot}`
    : b(`${ro} — macOS auto fans.`, `${ro} · macOS 自动风扇。`);
  return {
    status: st, rpmText, showController: true, controller,
    badgeText: y ? b("Yield", "已让位") : b("Observer", "只读监测"), badgeKind: y ? "wn" : "in",
    tooltip: y ? `${ro}: ${controller}` : b(`${ro}: zero SMC writes`, `${ro}：零写入`),
    caption,
  };
}
