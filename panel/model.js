// Pure locale, policy, topology, display-guard, and read-only fan ownership helpers.

export function normalizeLang(raw) {
  const s = String(raw ?? "").trim().toLowerCase();
  if (!s || s === "auto") return null;
  return s === "zh" || s.startsWith("zh-") || s.startsWith("zh_") ? "zh-CN" : "en";
}

export function resolvePanelLang({ saved, hostLocale, navLang, bootLocale } = {}) {
  for (const src of [saved, hostLocale, navLang, bootLocale]) {
    const n = normalizeLang(src);
    if (n) return n;
  }
  return "en";
}

export function computeOptimisticState(base, patch) {
  if (!base) return base;
  const s = { ...base.settings, ...patch };
  const total = Math.max(1, Number(base.cpuTopology?.total) || 8);
  const bp = base.policy || {};
  let autoProfile = bp.autoProfile || "normal";
  if (patch?.acClosedProfile && !["warn", "serious", "critical"].includes(base.thermalState) && base.lidState === "closed" && base.powerSource !== "battery") {
    autoProfile = s.acClosedProfile === "eco" ? "eco" : "utility";
  }
  const ov = s.modeOverride || "auto", manual = ov !== "auto", profile = manual ? ov : autoProfile;
  const pj = base.profileJobs, tj = pj ? (!manual && profile === "eco" && pj.autoEco != null ? pj.autoEco : pj[profile]) : null;
  const maxJobs = s.maxJobsOverride > 0 ? Math.min(s.maxJobsOverride, total) : tj ?? (profile === "strict" ? 1 : profile === "normal" ? total : bp.maxJobs || total);
  let guardStatus = base.guardStatus || "armed";
  if (s.guardEnabled === false) guardStatus = "paused";
  else if (guardStatus === "paused") guardStatus = base.lidState === "closed" ? "protecting" : "armed";
  return { ...base, settings: s, guardStatus, policy: { ...bp, profile, autoProfile, modeSource: manual ? "manual" : "auto", maxJobs }, envVars: profile === "normal" ? {} : base.envVars };
}

export function describeGuardStatus(d, restorePct, lang = "en") {
  const zh = lang === "zh-CN";
  const st = d?.settings?.guardEnabled === false ? "paused" : d?.guardStatus || (d?.displayGuard ? "armed" : "idle");
  if (st === "paused") return zh ? "守护已暂停" : "Guard paused";
  if (st === "protecting") return zh ? `熄屏保护中 · 恢复 ${restorePct}%` : `Protecting · restores ${restorePct}%`;
  if (st === "thermal_cap") {
    const p = d?.displayGuard?.preThermalBrightness != null ? Math.round(d.displayGuard.preThermalBrightness * 100) : restorePct;
    return zh ? `温控限亮中 · 恢复 ${p}%` : `Thermal cap · restores ${p}%`;
  }
  return st === "armed" ? (zh ? "守护运行中" : "Guard active") : zh ? "守护待命" : "Guard idle";
}

const rpm = (n) => Math.max(0, Math.round(Number(n) || 0)).toLocaleString("en-US");

export function describeFanState(fan, lang = "en") {
  const zh = lang === "zh-CN", st = fan?.status || "unavailable", fs = Array.isArray(fan?.fans) ? fan.fans : [];
  const ex = Array.isArray(fan?.externalManagers) ? fan.externalManagers : [], y = fan?.ownershipMode === "yield";
  const rc = fan?.controller || "System (macOS)";
  const controller = rc === "System (macOS)" ? (zh ? "系统自动 (macOS)" : rc) : rc === "Manual (SMC)" ? (zh ? "手动策略 (SMC)" : rc) : rc;
  if (st === "fanless") {
    return { status: st, rpmText: zh ? "无风扇（被动散热）" : "No fans (passive cooling)", showController: false, controller, badgeText: zh ? "只读监测" : "Observer", badgeKind: "in", tooltip: zh ? "无风扇被动散热" : "Passive cooling", caption: zh ? "无风扇被动散热，协同限流与熄屏守护。" : "Passive cooling — throttling & guard active." };
  }
  const rpmText = st === "ok" && fs.length === 2
    ? (zh ? `左 ${rpm(fs[0].rpm)} · 右 ${rpm(fs[1].rpm)} rpm` : `L ${rpm(fs[0].rpm)} · R ${rpm(fs[1].rpm)} rpm`)
    : st === "ok" && fs.length ? `${fs.map((f) => rpm(f.rpm)).join(" · ")} rpm`
    : zh ? "传感器不可读" : "Unavailable";
  const badgeText = y ? (zh ? "已让位" : "Yield") : zh ? "只读监测" : "Observer";
  const badgeKind = y ? "wn" : "in";
  const tooltip = y ? (zh ? `只读无冲突：让位给 ${controller}` : `Read-only: yields to ${controller}`) : zh ? "只读监测：零 SMC 写入" : "Read-only: zero SMC writes";
  const caption = st === "unavailable"
    ? (zh ? "风扇传感器不可读；限流与守护正常。" : "Fan sensor unavailable; throttling & guard active.")
    : y && ex.length ? (zh ? `只读模式：已让位给 ${controller}。` : `Read-only — yields to ${controller}.`)
    : y ? (zh ? "只读模式：手动风扇，已让位控制。" : "Read-only — manual fan mode; yields control.")
    : zh ? "只读监测 macOS 自动风扇，零写入。" : "Read-only — observes macOS auto fans (zero writes).";
  return { status: st, rpmText, showController: true, controller, badgeText, badgeKind, tooltip, caption };
}
