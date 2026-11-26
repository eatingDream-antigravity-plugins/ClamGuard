import { App, applyHostContext } from "./bridge.js";
import { attachRevealRefresh, createAdaptivePoller } from "./polling.js";
import { clampPercent, createBrightnessController } from "./brightness.js";
import { computeOptimisticState, describeFanState, describeGuardStatus, resolvePanelLang } from "./model.js";

const $ = (id) => document.getElementById(id);
const root = $("app");
const boot = typeof window !== "undefined" ? window.__MAC_CLAMSHELL_BOOT__ || null : null;
const PRESETS = [0, 20, 50, 80, 100];
const MODES = { auto: ["Auto", "自动"], normal: ["Normal", "正常"], utility: ["Utility", "控温"], eco: ["Eco", "节能"], strict: ["Strict", "强限"] };
const MODE_IDS = Object.keys(MODES);
const LKEY = "mac-clamshell-lang", DK = "data-k", AC = "aria-checked", AD = "aria-disabled", DM = "data-m", DBR = "data-br", DAC = "data-ac";

const TXT = "k0|Lid|盒盖;k1|Power|电源;k2|Thermal|温控;k3|Guard|守护;k4|Built-in Display|内建屏幕亮度;k5|Closed-lid screen guard|合盖熄屏守护;k6|Lock Screen|锁屏;k7|Performance Mode|性能档位;k8|Parallel Jobs|并发上限;ar|Reset|恢复自动;k9|Fans|风扇与散热;ka|Controller|控制方;kb|System Details|系统详情;kc|Cores & Memory|核心与内存;kd|Load & Power|负载与电源;ke|Closed on AC power|合盖插电默认档位;emp-btn|Retry|重试;bnr-act|Retry|重试"
  .split(";").map((r) => r.split("|"));
const ARIA = "#auto-chip|Auto-refresh|自动轮询;#lang-grp|Language|语言;#lock-btn|Lock Screen|锁屏;#refresh-btn|Refresh|刷新;#bnr-x|Dismiss|关闭;#jd|Fewer jobs|减少并发;#ji|More jobs|增加并发;[data-bs='-5']|-5%|-5%;[data-bs='5']|+5%|+5%"
  .split(";").map((r) => r.split("|"));

const storeLang = (v) => { try { return typeof localStorage !== "undefined" ? (v === undefined ? localStorage.getItem(LKEY) : localStorage.setItem(LKEY, v)) : null; } catch { return null; } };
let hostLoc = null, lang = resolvePanelLang({ saved: storeLang(), navLang: typeof navigator !== "undefined" ? navigator.language : null, bootLocale: boot?.locale });
document.documentElement.lang = lang;

const zh = () => lang === "zh-CN", bi = (e, z) => (zh() ? z : e), rty = () => bi("Retry", "重试"), mName = (id) => (MODES[id] ? MODES[id][zh() ? 1 : 0] : id);

let cur = boot, conf = boot, hash = boot?.hash || "", loading = !boot, first = Boolean(!boot || boot?.stale);
let syncing = false, bSync = false, locking = false, optB = null, fb = null, fbT = null, netM = null, envK = "";
let pend = null, inF = false, gen = 0, drag = false, defr = false, busyT = null, spin = false;
let fails = 0, off = false, bnrX = false, bnrE = null;

const app = new App({ name: "Mac Clamshell", version: "1.0.0" }, {});
const num = (v, d = 0) => (Number.isFinite(Number(v)) ? Number(v) : d);
const qa = (s) => root?.querySelectorAll?.(s) || [];
const ga = (e, k) => e?.getAttribute?.(k) ?? null;
const sa = (e, k, v) => { const s = String(v); if (e && e.getAttribute(k) !== s) e.setAttribute(k, s); };
const tx = (e, v) => { const el = typeof e === "string" ? $(e) : e, s = String(v ?? ""); if (el && el.textContent !== s) el.textContent = s; };
const vis = (e, on) => { const el = typeof e === "string" ? $(e) : e; if (el && el.hidden !== !on) el.hidden = !on; };

function mountControls() {
  if (typeof document.createElement !== "function") return;
  for (const [id, arr, attr, fn] of [["prs-grp", PRESETS, DBR, (n) => `${n}%`], ["mode-rail", MODE_IDS, DM], ["ac-grp", ["utility", "eco"], DAC]]) {
    const h = $(id); if (!h?.appendChild) continue;
    for (const v of arr) { const b = document.createElement("button"); b.type = "button"; sa(b, "role", "radio"); sa(b, attr, v); if (fn) b.textContent = fn(v); h.appendChild(b); }
  }
}

const fmtUp = (s) => {
  if (!Number.isFinite(s) || s <= 0) return "-";
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60), [du, hu, mu] = zh() ? ["天 ", "时 ", "分"] : ["d ", "h ", "m"];
  return d > 0 ? `${d}${du}${h}${hu.trim()}` : h > 0 ? `${h}${hu}${m}${mu}` : `${m}${mu}`;
};
const fmtT = (iso) => { if (!iso) return "-"; const d = new Date(iso); return Number.isNaN(d.getTime()) ? iso : d.toTimeString().slice(0, 8); };
const lidS = (l) => ({ closed: bi("Closed", "已合盖"), open: bi("Open", "已开盖") }[l] || bi("Unknown", "未知"));
const pwrS = (p, pct) => { const l = { battery: bi("Battery", "电池"), ac: bi("AC", "插电") }[p]; return l ? l + (pct != null ? ` ${pct}%` : "") : bi("Unknown", "未知"); };
const thInfo = (d) => {
  const p = d?.thermalPressure;
  return p === "critical" ? [bi("Critical", "严重过热"), "er"] : p === "serious" || d?.thermalState === "warn" ? [bi("Serious", "偏高降频"), "er"] : p === "fair" ? [bi("Fair", "轻度温升"), "wn"] : p === "unknown" || d?.thermalState === "unknown" ? [bi("Unknown", "未知"), "t3"] : [bi("Nominal", "正常"), "ok"];
};
const grHero = (d) => {
  const st = d?.settings?.guardEnabled === false ? "paused" : d?.guardStatus || "idle";
  return st === "paused" ? [bi("Paused", "已暂停"), "wn", true] : st === "protecting" ? [bi("Protecting", "保护中"), "ok", false] : st === "thermal_cap" ? [bi("Thermal cap", "温控限亮"), "wn", false] : st === "armed" ? [bi("Active", "运行中"), "ok", false] : [bi("Idle", "待命"), "t3", false];
};
const effB = (d) => optB !== null ? optB : !d?.display ? null : (d.lidState === "closed" || d.display.asleep ? d.display.savedPercent ?? d.display.percent ?? 20 : d.display.percent ?? d.display.savedPercent ?? null);

function setFb(k, t, retry = null) {
  fb = { k, t, retry }; clearTimeout(fbT);
  if (k === "ok") fbT = setTimeout(() => { fb = null; patchFb(); }, 2400);
  patchFb();
}
function patchFb() {
  const p = $("fb-p"); if (!p) return;
  if (syncing || bSync) { vis(p, 1); sa(p, DK, "sync"); vis("fb-d", 1); tx("fb-t", bi("Saving…", "同步中…")); vis("fb-r", 0); }
  else if (fb) { const r = fb.k === "err" && !!fb.retry; vis(p, 1); sa(p, DK, fb.k); vis("fb-d", 0); tx("fb-t", fb.t); vis("fb-r", r); if (r) tx("fb-r", rty()); }
  else vis(p, 0);
}
function patchBnr() {
  const b = $("bnr"); if (!b) return;
  if ((bnrE || off) && !bnrX) {
    const s = Math.max(1, Math.round(((netM || poller.getMetrics()).currentIntervalMs || 5000) / 1000));
    vis(b, 1); sa(b, DK, bnrE ? "er" : "wn");
    tx("bnr-t", bnrE || bi(`Connection lost · retrying in ${s}s`, `连接中断 · ${s}s 后重试`));
    vis("bnr-act", 1); tx("bnr-act", rty());
  } else vis(b, 0);
}
function onOk() { const w = off; fails = 0; off = bnrX = first = false; bnrE = null; patchBnr(); if (w) setFb("ok", bi("✓ Reconnected", "✓ 已重新连接")); }
function onFail(r) { first = false; if (r === "poll" && ++fails >= 3) { off = true; bnrX = false; } patchBnr(); }

function applyData(p) {
  if (!p || typeof p !== "object") return;
  if (p.unchanged) {
    if (cur && p.timestamp) { cur = { ...cur, timestamp: p.timestamp }; if (conf) conf = { ...conf, timestamp: p.timestamp }; }
    if (p.hash) hash = p.hash;
    return;
  }
  conf = p; if (p.hash) hash = p.hash;
  cur = syncing && cur?.settings ? computeOptimisticState(p, cur.settings) : p;
}

function patchI18n() {
  document.documentElement.lang = lang;
  for (const [id, e, z] of TXT) tx(id, bi(e, z));
  for (const [sel, e, z] of ARIA) sa(root?.querySelector?.(sel), "aria-label", bi(e, z));
  for (const b of qa(`[${DM}]`)) tx(b, mName(ga(b, DM)));
  for (const b of qa(`[${DAC}]`)) tx(b, mName(ga(b, DAC)));
  for (const b of qa("[data-lg]")) sa(b, "aria-pressed", ga(b, "data-lg") === lang);
}
const showEmpty = (m, r = false) => { vis("shell", 0); vis("emp", 1); tx("emp-msg", m); vis("emp-btn", r); };

function patchChrome() {
  const net = netM || poller.getMetrics();
  if (cur?.timestamp) tx("ht", fmtT(cur.timestamp));
  tx("ft", net.inBackground ? bi("Background", "后台") : "");
  const paused = Boolean(cur && (cur.settings?.guardEnabled === false || cur.guardStatus === "paused")), ch = $("auto-chip");
  if (ch) {
    const s = Math.round((net.currentIntervalMs || 3000) / 1000);
    sa(ch, "data-on", net.enabled); sa(ch, "data-ps", paused); sa(ch, "aria-pressed", net.enabled);
    ch.title = cur?.timestamp ? bi(`Last sync ${fmtT(cur.timestamp)}`, `上次同步 ${fmtT(cur.timestamp)}`) : "";
    tx($("auto-lbl") || ch.lastElementChild, bi(`Auto ${s}s`, `自动 ${s}s`));
  }
  if ($("lock-btn")) $("lock-btn").disabled = locking;
  sa($("refresh-btn"), "data-busy", spin);
  vis("swp", first && cur);
  patchFb(); patchBnr();
}

function patchBright(pct, ok = true) {
  tx("bv", ok ? `${pct}%` : "-");
  const tr = $("bright-track");
  if (tr) { tr.style.setProperty("--pct", `${pct}%`); sa(tr, "aria-valuenow", pct); sa(tr, AD, !ok); tr.tabIndex = ok ? 0 : -1; }
  for (const b of qa(`[data-bs],[${DBR}]`)) { b.disabled = !ok; const v = ga(b, DBR); if (v !== null) sa(b, AC, ok && Number(v) === pct); }
}

function patchEnv(d, prof, het, nice) {
  const el = $("de"); if (!el) return;
  const es = Object.entries(d.envVars || {}), qos = prof === "utility" ? "Utility QoS" : het ? "E-Core QoS" : "Background QoS";
  const k = `${lang}|${qos}|${nice}|${es.map((x) => x.join("=")).join(";")}`;
  if (k === envK) return; envK = k;
  const none = bi("No process limits.", "无活跃限流参数。");
  if (!el.replaceChildren || typeof document.createElement !== "function") return tx(el, none);
  const mk = (pre, val) => { const s = document.createElement("span"); s.className = "et"; if (pre) s.appendChild(document.createTextNode(pre)); const b = document.createElement("strong"); b.textContent = String(val); s.appendChild(b); return s; };
  if (!es.length) { const s = document.createElement("span"); s.className = "cs"; s.textContent = none; return el.replaceChildren(s); }
  el.replaceChildren(mk("", qos), mk("nice ", nice), ...es.map(([x, v]) => mk(`${x}=`, v)));
}

const setBg = (id, t, k = "in", tip = "") => { const e = $(id); if (!e) return; vis(e, !!t); if (t) { sa(e, DK, k); tx(e, t); e.title = tip; } };

function patchAll(d) {
  if (!d) return; if (drag) { defr = true; return; }
  defr = false; vis("shell", 1); vis("emp", 0);

  const p = d.policy || {}, s = d.settings || {}, disp = d.display || {}, h = d.host || {}, rm = h.memory || {};
  const ov = s.modeOverride || "auto", prof = p.profile || "normal", pL = mName(prof), closed = d.lidState === "closed";
  const bOk = Boolean(disp.available || closed), bV = effB(d) ?? 20, rest = disp.savedPercent ?? bV;
  const [thL, thK] = thInfo(d), [grL, grK, grP] = grHero(d);

  sa($("hd"), "data-st", off || prof === "strict" || thK === "er" || d.degraded ? "err" : grP || prof === "utility" || prof === "eco" || thK === "wn" ? "wait" : "run");
  const bp = Math.max(0, Math.min(100, num(d.batteryPct, 0))), bk = d.powerSource === "ac" ? "ok" : bp <= 20 ? "er" : bp <= 40 ? "wn" : "ok";
  for (const [id, k, t] of [["h-lid", d.lidState === "open" ? "ok" : closed ? "ac" : "t3", lidS(d.lidState)], ["h-pwr", bk, pwrS(d.powerSource, d.batteryPct)], ["h-thm", thK, thL], ["h-grd", grK, grL]]) {
    sa($(id + "-d"), DK, k); tx(id, t);
  }
  vis("hbb", d.batteryPct != null);
  const hbf = $("hbf"); if (hbf) { sa(hbf, DK, bk); hbf.style.width = `${bp}%`; }

  setBg("bt", closed ? bi("Restore target", "恢复目标") : disp.asleep ? bi("Asleep", "已熄屏") : "", closed ? "ac" : "in");
  patchBright(bV, bOk);
const cap = !bOk ? bi("No built-in display", "无内建屏幕") : closed ? bi(`Closed · restores ${rest}%`, `已合盖 · 恢复 ${rest}%`) : disp.asleep ? bi(`Asleep · restores ${rest}%`, `已熄屏 · 恢复 ${rest}%`) : d.displayGuard?.thermalCapped ? bi("Thermal brightness cap", "温控限亮中") : "";
  vis("bc", !!cap);

  const gOn = s.guardEnabled !== false, gb = $("guard-btn");
  if (gb) { sa(gb, "data-on", gOn); sa(gb, "data-ps", grP); sa(gb, AC, gOn); }
  const rail = $("mode-rail");
  if (rail) {
    rail.style.setProperty("--idx", String(Math.max(0, MODE_IDS.indexOf(ov))));
    for (const b of qa(`[${DM}]`)) { const sel = ga(b, DM) === ov; sa(b, AC, sel); b.tabIndex = sel ? 0 : -1; }
  }
  setBg("mb", p.modeSource === "manual" ? bi("Manual", "手动") : prof !== "normal" ? pL : "", p.modeSource === "manual" ? "ac" : prof === "strict" ? "er" : "wn");
  const tot = num(d.cpuTopology?.total, 8), jobs = num(p.maxJobs, tot), fi = describeFanState(d.fan, lang);
  vis("ar", s.maxJobsOverride > 0);
  if ($("jd")) $("jd").disabled = jobs <= 1;
  if ($("ji")) $("ji").disabled = jobs >= tot;
  setBg("fth", thL, thK); vis("frow2", fi.showController);
  if (fi.showController) { tx("fctrl", fi.controller); setBg("fown", fi.badgeText, fi.badgeKind, fi.tooltip); }
  const pC = num(d.cpuTopology?.performanceCores, 0), eC = num(d.cpuTopology?.efficiencyCores, 0), het = pC > 0;
  const topo = het ? `${pC}P+${eC}E` : bi(`${tot} cores`, `${tot} 核`), memP = Math.max(0, Math.min(100, Math.round(num(rm.usedPct, 0))));
  const up = bi(`Up ${fmtUp(h.uptimeSeconds)}`, `运行 ${fmtUp(h.uptimeSeconds)}`);
  for (const [id, v] of [
    ["hs", `${lidS(d.lidState)} · ${pwrS(d.powerSource, d.batteryPct)} · ${pL}`], ["bc", cap], ["gs", describeGuardStatus(d, rest, lang)],
    ["sv", s.maxJobsOverride > 0 ? bi(`${jobs} ${jobs === 1 ? "job" : "jobs"}`, `${jobs} 并发`) : bi(`Auto · ${jobs}`, `自动 · ${jobs}`)],
    ["frpm", fi.rpmText], ["fcap", fi.caption], ["tp", `${topo} · ${bi(`Mem ${memP}%`, `内存 ${memP}%`)} · ${thL}`],
    ["dmu", num(rm.usedGb, 0)], ["dmt", `/ ${num(rm.totalGb, 0)} GB (${memP}%)`],
    ["dc", `${topo} · ${bi(`${num(rm.compressedGb, 0)} GB comp.`, `压缩 ${num(rm.compressedGb, 0)} GB`)}`],
    ["dld", `${bi("Load", "负载")} ${(h.loadAvg || []).join(" · ")}`],
    ["dsp", d.cpuSpeedLimitPct != null ? `${bi("CPU", "CPU")} ${num(d.cpuSpeedLimitPct, 100)}% · ${up}` : up],
    ["ds", d.clamshellCausesSleep === "no" ? bi("Clamshell awake", "合盖不休眠") : d.clamshellCausesSleep === "yes" ? bi("Sleeps on close", "合盖休眠") : "—"],
    ["dlm", `${bi("Low Power:", "低电量:")} ${d.lowPowerMode === "1" ? bi("On", "开") : d.lowPowerMode === "0" ? bi("Off", "关") : "—"}`],
    ["dhi", `${h.hostname || "Mac"} · ${h.chip || "Mac"} · macOS ${h.macosVersion || ""}`],
  ]) tx(id, v);
  const dmf = $("dmf"); if (dmf) { sa(dmf, DK, memP >= 85 ? "er" : memP >= 60 ? "wn" : "ok"); dmf.style.width = `${memP}%`; }
  for (const b of qa(`[${DAC}]`)) sa(b, AC, ga(b, DAC) === (s.acClosedProfile || "utility"));
  patchEnv(d, prof, het, p.niceLevel ?? 0);
  if ($("dh")) $("dh").title = d.paths?.settingsPath || "";
  patchChrome();
}

async function rpc(name, args, track = true) {
  const t0 = performance.now(), res = await app.callServerTool({ name, arguments: args });
  if (res?.isError || !res?.structuredContent) throw new Error(res?.content?.[0]?.text || "err");
  const sc = res.structuredContent;
  if (track) { poller.recordExternalRpc({ totalMs: Math.round(performance.now() - t0), serverMs: sc.serverMs ?? 0, markSuccess: true }); onOk(); }
  return sc;
}

const poller = createAdaptivePoller({
  defaultEnabled: true,
  shouldSkipBackgroundPoll: () => syncing || bSync || drag || inF,
  onMetrics: (m) => { netM = m; patchChrome(); },
  fetcher: async ({ force, reason }) => {
    if (force) { loading = true; clearTimeout(busyT); busyT = setTimeout(() => { if (loading) { spin = true; patchChrome(); } }, 150); }
    try {
      const sc = await rpc("clamshell_refresh", { force: !!force, ...(force ? { fresh: true } : {}), ...(hash ? { ifHash: hash } : {}) }, false);
      onOk(); applyData(sc);
      if (!sc.unchanged) patchAll(cur); else patchChrome();
      return sc;
    } catch (err) {
      onFail(reason);
      if (reason === "manual") setFb("err", bi("Refresh failed", "刷新失败"), () => refreshNow(true));
      throw err;
    } finally {
      if (force) { clearTimeout(busyT); loading = spin = false; patchChrome(); }
    }
  },
});

const bCtrl = createBrightnessController({
  setter: (percent) => rpc("clamshell_set_brightness", { percent }),
  onOptimistic: (p) => { optB = p; patchBright(p, true); },
  onSyncChange: (s) => { bSync = s; patchFb(); },
  onCommit: (sc, target) => {
    optB = null; if (sc) applyData(sc);
    const a = sc?.brightnessAction, t = a?.mode === "saved_target", v = t ? a.savedPercent ?? target : a?.percent ?? target;
    patchAll(cur);
    setFb("ok", t ? bi(`✓ Restores to ${v}%`, `✓ 开盖恢复 ${v}%`) : bi(`✓ Brightness ${v}%`, `✓ 亮度 ${v}%`));
  },
  onRollback: (_e, target) => {
    optB = null; patchBright(effB(cur) ?? 20, Boolean(cur?.display?.available || cur?.lidState === "closed"));
    setFb("err", bi("Brightness update failed", "亮度调节失败"), () => bCtrl.schedule(target, { immediate: true }));
  },
});

const refreshNow = (force = true) => poller.trigger({ force, reason: force ? "manual" : "poll" }).catch(() => {});

async function lockNow() {
  if (locking) return;
  locking = true; patchChrome();
  try { await rpc("clamshell_lock_screen", {}); setFb("ok", bi("✓ Locked", "✓ 已锁屏")); }
  catch (e) { setFb("err", e?.message || bi("Lock failed", "锁屏失败"), lockNow); }
  finally { locking = false; patchChrome(); }
}

async function flushQ() {
  if (inF || !pend) return;
  inF = syncing = true; patchFb();
  try {
    while (pend) {
      const patch = pend, g = gen; pend = null;
      try {
        const sc = await rpc("clamshell_configure", patch);
        conf = sc; if (sc.hash) hash = sc.hash;
        const ok = g === gen && !pend;
        cur = ok ? sc : cur?.settings ? computeOptimisticState(sc, cur.settings) : sc;
        if (ok) { patchAll(cur); setFb("ok", bi("✓ Saved", "✓ 已保存")); }
      } catch {
        if (pend) { pend = { ...patch, ...pend }; continue; }
        cur = conf; patchAll(cur); setFb("err", bi("Save failed", "保存失败"), () => queueCfg(patch)); break;
      }
    }
  } finally { inF = false; syncing = !!pend; patchFb(); if (pend) flushQ(); }
}

function queueCfg(p) {
  gen += 1; pend = { ...(pend || {}), ...p }; syncing = true;
  if (cur) { cur = computeOptimisticState(cur, p); patchAll(cur); }
  flushQ();
}

function syncCtx(ctx) {
  applyHostContext(ctx);
  if (ctx?.locale && ctx.locale !== hostLoc) {
    hostLoc = ctx.locale;
    if (!storeLang()) {
      const n = resolvePanelLang({ saved: null, hostLocale: hostLoc, navLang: typeof navigator !== "undefined" ? navigator.language : null, bootLocale: cur?.locale });
      if (n !== lang) { lang = n; patchI18n(); if (cur) patchAll(cur); }
    }
  }
}

const pctPtr = (tr, x) => { const r = tr.getBoundingClientRect(); return r.width ? clampPercent(((x - r.left) / r.width) * 100) : effB(cur) ?? 20; };

root?.addEventListener("click", (e) => {
  const b = e.target?.closest?.("button"); if (!b || b.disabled) return;
  const a = ga(b, "data-a");
  if (a === "refresh" || a === "bnr-retry") return refreshNow(true);
  if (a === "lock") return lockNow();
  if (a === "auto") return poller.setEnabled(!poller.getMetrics().enabled);
  if (a === "guard") return queueCfg({ guardEnabled: !(cur?.settings?.guardEnabled !== false) });
  if (a === "retry") return fb?.retry?.();
  if (a === "bnr-dismiss") { bnrX = true; patchBnr(); return; }
  if (a === "jauto") return queueCfg({ maxJobsOverride: 0 });
  const lg = ga(b, "data-lg"); if (lg && lg !== lang) { lang = lg; storeLang(lg); patchI18n(); if (cur) patchAll(cur); return queueCfg({ lang: lg }); }
  const bs = ga(b, "data-bs"); if (bs !== null) return bCtrl.schedule(clampPercent((effB(cur) ?? 20) + Number(bs)), { immediate: true });
  const br = ga(b, DBR); if (br !== null) return bCtrl.schedule(Number(br), { immediate: true });
  const m = ga(b, DM); if (m) return queueCfg({ modeOverride: m });
  const js = ga(b, "data-js"); if (js !== null) { const t = num(cur?.cpuTopology?.total, 8); return queueCfg({ maxJobsOverride: Math.max(1, Math.min(t, num(cur?.policy?.maxJobs, t) + Number(js))) }); }
  const ac = ga(b, DAC); if (ac) queueCfg({ acClosedProfile: ac });
});

root?.addEventListener("pointerdown", (e) => {
  const tr = e.target?.closest?.("#bright-track"); if (!tr || ga(tr, AD) === "true") return;
  drag = true; sa(tr, "data-drag", "true"); tr.setPointerCapture?.(e.pointerId); bCtrl.schedule(pctPtr(tr, e.clientX), { immediate: false });
});
root?.addEventListener("pointermove", (e) => { if (drag && $("bright-track")) bCtrl.schedule(pctPtr($("bright-track"), e.clientX), { immediate: false }); });
function endDrag(e) {
  if (!drag) return; drag = false; const tr = $("bright-track"); tr?.removeAttribute("data-drag");
  if (tr && typeof e?.clientX === "number") bCtrl.schedule(pctPtr(tr, e.clientX), { immediate: true }); else bCtrl.flush();
  if (defr && cur) patchAll(cur);
}
const cancelDrag = () => endDrag();
window.addEventListener("pointerup", endDrag);
window.addEventListener("pointercancel", cancelDrag);

root?.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { const t = $("diag-tray"); if (t?.open) { t.open = false; t.querySelector("summary")?.focus(); e.preventDefault(); } return; }
  if (e.target?.id === "bright-track") {
    if (ga(e.target, AD) === "true") return;
    const c = effB(cur) ?? 20, s = e.shiftKey ? 10 : 1;
    const d = { ArrowRight: s, ArrowUp: s, ArrowLeft: -s, ArrowDown: -s, PageUp: 10, PageDown: -10, Home: -c, End: 100 - c }[e.key];
    if (d !== undefined) { e.preventDefault(); bCtrl.schedule(clampPercent(c + d), { immediate: false }); }
    return;
  }
  const dir = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0; if (!dir) return;
  const mb = e.target?.closest?.(`[${DM}]`), pb = e.target?.closest?.(`[${DBR}]`);
  if (mb) { e.preventDefault(); const n = MODE_IDS[(MODE_IDS.indexOf(ga(mb, DM)) + dir + 5) % 5]; queueCfg({ modeOverride: n }); root.querySelector(`[${DM}="${n}"]`)?.focus(); }
  else if (pb) { e.preventDefault(); const n = PRESETS[(PRESETS.indexOf(Number(ga(pb, DBR))) + dir + 5) % 5]; bCtrl.schedule(n, { immediate: true }); root.querySelector(`[${DBR}="${n}"]`)?.focus(); }
});

const detach = attachRevealRefresh({ doc: document, win: window, target: document.documentElement, poller });
app.ontoolinput = () => { if (!cur) { loading = true; showEmpty(bi("Reading Mac status…", "正在读取设备状态…")); } };
app.ontoolresult = (r) => {
  const sc = r?.structuredContent; if (!sc || typeof sc !== "object" || (!sc.unchanged && !sc.policy && !sc.host)) return;
  const prev = hash, had = !!cur; onOk(); applyData(sc); loading = false; poller.markInitialBoot();
  if (had && (sc.unchanged || (sc.hash && sc.hash === prev))) patchChrome(); else patchAll(cur);
};
app.onhostcontextchanged = (ctx) => { syncCtx(ctx); if ((ctx?.containerDimensions || ctx?.displayMode) && !drag && !bSync) poller.forceNow("host-context").catch(() => {}); };
app.onteardown = () => { clearTimeout(busyT); clearTimeout(fbT); detach(); poller.destroy(); bCtrl.destroy(); window.removeEventListener("pointerup", endDrag); window.removeEventListener("pointercancel", cancelDrag); return {}; };

(async () => {
  mountControls(); patchI18n();
  if (boot) { poller.markInitialBoot(); patchAll(boot); } else showEmpty(bi("Reading Mac status…", "正在读取设备状态…"));
  try {
    await app.connect(); syncCtx(app.getHostContext()); patchChrome();
    if (boot?.stale) poller.trigger({ force: false, reason: "swr" }).catch(() => {});
    else if (!cur) setTimeout(() => { if (!cur) refreshNow(true); }, 150);
  } catch (e) {
    loading = first = false;
    if (!cur) showEmpty(`${bi("Initialization failed: ", "初始化失败：")}${e?.message || e}`, true);
    else { bnrE = bi("Host disconnected", "宿主连接异常"); bnrX = false; patchBnr(); }
  }
})();
