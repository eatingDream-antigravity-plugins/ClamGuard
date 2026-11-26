import { App } from "./bridge.js";
import { attachRevealRefresh, createAdaptivePoller } from "./polling.js";
import { clampPercent, createBrightnessController } from "./brightness.js";
import { computeOptimisticState, describeFanState, describeGuardStatus, resolvePanelLang } from "./model.js";

const doc = document, docEl = doc.documentElement, $ = (id) => doc.getElementById(id), root = $("app"), st = setTimeout, ct = clearTimeout, M = Math;
const boot = typeof window < "u" ? window.__MAC_CLAMSHELL_BOOT__ || null : null;
const PRESETS = [0, 20, 50, 80, 100], MODE_IDS = ["auto", "normal", "utility", "eco", "strict"];
const MODES = Object.fromEntries("auto|Auto|自动;normal|Normal|正常;utility|Utility|控温;eco|Eco|节能;strict|Strict|强限".split(";").map((r) => { const [k, e, z] = r.split("|"); return [k, [e, z]]; }));
const LKEY = "mac-clamshell-lang", D = "data-", DK = D + "k", DM = D + "m", DBR = D + "br", DAC = D + "ac", DON = D + "on", DPS = D + "ps", DA = D + "a", DLG = D + "lg", DBS = D + "bs", DJS = D + "js", DST = D + "st", DBU = D + "busy", DDR = D + "drag";
const AC = "aria-checked", AD = "aria-disabled", AP = "aria-pressed", AR = "ArrowRight", AL = "ArrowLeft", PU = "pointerup", PC = "pointercancel";

const TXT = "k0|Lid|盒盖;k1|Power|电源;k2|Thermal|温控;k3|Guard|守护;k4|Display|亮度;k5|Lid guard|合盖守护;k6|Lock|锁屏;k7|Mode|档位;k8|Jobs|并发;ar|Reset|自动;k9|Fans|风扇;ka|Controller|控制方;kb|Details|详情;kc|Cores & Mem|核心内存;kd|Load & Power|负载电源;ke|Closed on AC|合盖插电"
  .split(";").map((r) => r.split("|"));
const ARIA = "#auto-chip|Auto|自动轮询;#lang-grp|Lang|语言;#lock-btn|Lock|锁屏;#refresh-btn|Refresh|刷新;#bnr-x|Close|关闭;#jd|Fewer jobs|减少并发;#ji|More jobs|增加并发"
  .split(";").map((r) => r.split("|"));

const storeLang = (v) => { try { return typeof localStorage < "u" ? (v === undefined ? localStorage.getItem(LKEY) : localStorage.setItem(LKEY, v)) : null; } catch { return null; } };
const navL = () => (typeof navigator < "u" ? navigator.language : null);
let hostLoc = null, lang = resolvePanelLang({ saved: storeLang(), navLang: navL(), bootLocale: boot?.locale });
docEl.lang = lang;

const zh = () => lang === "zh-CN", bi = (e, z) => (zh() ? z : e), rty = () => bi("Retry", "重试"), mName = (id) => (MODES[id] ? MODES[id][zh() ? 1 : 0] : id);
const loadTxt = () => bi("Loading…", "读取中…"), unk = () => bi("Unknown", "未知"), asl = () => bi("Asleep", "已熄屏");

let cur = boot, conf = boot, hash = boot?.hash || "", loading = !boot, first = Boolean(!boot || boot?.stale);
let syncing = false, bSync = false, locking = false, optB = null, fb = null, fbT = null, netM = null, envK = "";
let pend = null, inF = false, gen = 0, drag = false, defr = false, busyT = null, spin = false;
let fails = 0, off = false, bnrX = false, bnrE = null, canTools = false, autoWant = true;

const num = (v, d = 0) => (Number.isFinite(+v) ? +v : d), qa = (s) => root?.querySelectorAll?.(s) || [], q1 = (s) => root?.querySelector?.(s);
const ga = (e, k) => e?.getAttribute?.(k) ?? null, sa = (e, k, v) => { const s = String(v); if (e && e.getAttribute(k) !== s) e.setAttribute(k, s); };
const app = new App({ name: "Mac Clamshell", version: ga(doc.querySelector?.('meta[name="app-version"]'), "content") || "1.4.0" }, {});
const tx = (e, v) => { const el = typeof e === "string" ? $(e) : e, s = String(v ?? ""); if (el && el.textContent !== s) el.textContent = s; };
const vis = (e, on) => { const el = typeof e === "string" ? $(e) : e; if (el && el.hidden !== !on) el.hidden = !on; };
const dis = (id, v) => { const e = $(id); if (e) e.disabled = v; }, on = (t, ev, fn) => t?.addEventListener(ev, fn);
const redraw = () => (cur ? patchAll(cur) : patchChrome()), btr = () => $("bright-track"), NOOP = () => {};
const gOff = (d) => d?.settings?.guardEnabled === false, hasD = (d) => Boolean(d?.display?.available || d?.lidState === "closed"), cS = (e, s) => e.target?.closest?.(s);

function mountControls() {
  if (!doc.createElement) return;
  for (const [id, arr, attr, fn] of [["prs-grp", PRESETS, DBR, (n) => `${n}%`], ["mode-rail", MODE_IDS, DM], ["ac-grp", ["utility", "eco"], DAC]]) {
    const h = $(id); if (!h?.appendChild) continue;
    for (const v of arr) { const b = doc.createElement("button"); b.type = "button"; sa(b, "role", "radio"); sa(b, attr, v); if (fn) b.textContent = fn(v); h.appendChild(b); }
  }
}

const fmtUp = (s) => {
  if (!Number.isFinite(s) || s <= 0) return "-";
  const d = (s / 86400) | 0, h = ((s % 86400) / 3600) | 0, m = ((s % 3600) / 60) | 0, [du, hu, mu] = zh() ? ["天 ", "时 ", "分"] : ["d ", "h ", "m"];
  return d > 0 ? `${d}${du}${h}${hu.trim()}` : h > 0 ? `${h}${hu}${m}${mu}` : `${m}${mu}`;
};
const fmtT = (iso) => { if (!iso) return "-"; const d = new Date(iso); return Number.isNaN(d.getTime()) ? iso : d.toTimeString().slice(0, 8); };
const lidS = (l) => ({ closed: bi("Closed", "已合盖"), open: bi("Open", "已开盖") }[l] || unk());
const pwrS = (p, pct) => { const l = { battery: bi("Battery", "电池"), ac: bi("AC", "插电") }[p]; return l ? l + (pct != null ? ` ${pct}%` : "") : unk(); };
const TH = { critical: ["Critical", "严重过热", "er"], serious: ["Serious", "偏高降频", "er"], fair: ["Fair", "轻度温升", "wn"], unknown: ["Unknown", "未知", "t3"] };
const thInfo = (d) => { const r = TH[d?.thermalPressure] || (d?.thermalState === "warn" ? TH.serious : d?.thermalState === "unknown" ? TH.unknown : ["Nominal", "正常", "ok"]); return [bi(r[0], r[1]), r[2]]; };
const GR = { paused: ["Paused", "已暂停", "wn", 1], protecting: ["Protecting", "保护中", "ok"], thermal_cap: ["Thermal cap", "温控限亮", "wn"], armed: ["Active", "运行中", "ok"] };
const grHero = (d) => { const r = GR[gOff(d) ? "paused" : d?.guardStatus || "idle"] || ["Idle", "待命", "t3"]; return [bi(r[0], r[1]), r[2], !!r[3]]; };
const effB = (d) => optB !== null ? optB : !d?.display ? null : (d.lidState === "closed" || d.display.asleep ? d.display.savedPercent ?? d.display.percent ?? 20 : d.display.percent ?? d.display.savedPercent ?? null);

function setFb(k, t, retry = null) { fb = { k, t, retry }; ct(fbT); if (k === "ok") fbT = st(() => { fb = null; patchFb(); }, 2400); patchFb(); }
const setOk = (e, z) => setFb("ok", `✓ ${bi(e, z)}`), fail = (e, z, r) => setFb("err", bi(`${e} failed`, `${z}失败`), r);
function patchFb() {
  const p = $("fb-p"); if (!p) return;
  if (syncing || bSync) { vis(p, 1); sa(p, DK, "sync"); vis("fb-d", 1); tx("fb-t", bi("Saving…", "同步中…")); vis("fb-r", 0); }
  else if (fb) { const r = fb.k === "err" && !!fb.retry; vis(p, 1); sa(p, DK, fb.k); vis("fb-d", 0); tx("fb-t", fb.t); vis("fb-r", r); if (r) tx("fb-r", rty()); }
  else vis(p, 0);
}
function patchDot(d = cur) {
  const p = d?.policy?.profile || "normal", thK = d ? thInfo(d)[1] : "ok", grP = d ? grHero(d)[2] : false;
  sa($("hd"), DST, off || bnrE || p === "strict" || thK === "er" || d?.degraded ? "err" : grP || p === "utility" || p === "eco" || thK === "wn" ? "wait" : "run");
}
function patchBnr() {
  const b = $("bnr"); if (!b) return;
  if ((bnrE || off) && !bnrX) {
    const net = netM || poller.getMetrics(), s = M.max(1, M.round((net.currentIntervalMs || 5e3) / 1e3));
    vis(b, 1); sa(b, DK, bnrE ? "er" : "wn");
    tx("bnr-t", bnrE || `${bi("Connection lost", "连接中断")} · ${canTools && net.enabled ? bi(`retrying in ${s}s`, `${s}s 后重试`) : bi("auto off", "自动轮询已关")}`);
    vis("bnr-act", 1); tx("bnr-act", rty());
  } else vis(b, 0);
}
function onOk() { const w = off || !!bnrE; fails = 0; off = bnrX = first = false; bnrE = null; patchDot(); patchBnr(); if (w) setOk("Reconnected", "已重新连接"); }
function onFail(r) { first = false; if (r === "poll" && ++fails >= 3) { off = true; bnrX = false; } patchDot(); patchBnr(); }

function applyData(p) {
  if (!p || typeof p !== "object") return;
  if (p.hash) hash = p.hash;
  if (p.unchanged) {
    const t = p.timestamp;
    if (cur && t) { cur = { ...cur, timestamp: t }; if (conf) conf = { ...conf, timestamp: t }; }
    return;
  }
  conf = p; cur = syncing && cur?.settings ? computeOptimisticState(p, cur.settings) : p;
}

function patchI18n() {
  docEl.lang = lang;
  for (const [id, e, z] of TXT) tx(id, bi(e, z));
  tx("emp-btn", rty()); tx("bnr-act", rty());
  for (const [sel, e, z] of ARIA) sa(q1(sel), "aria-label", bi(e, z));
  for (const b of qa(`[${DM}],[${DAC}]`)) tx(b, mName(ga(b, DM) || ga(b, DAC)));
  for (const b of qa(`[${DLG}]`)) sa(b, AP, ga(b, DLG) === lang);
}
const showEmpty = (m, r = false) => { vis("shell", 0); vis("emp", 1); tx("emp-msg", m); vis("emp-btn", r); };

function patchChrome() {
  const net = netM || poller.getMetrics(), ts = cur?.timestamp, paused = Boolean(cur && (gOff(cur) || cur.guardStatus === "paused")), ch = $("auto-chip"), en = canTools && net.enabled;
  if (ts) tx("ht", fmtT(ts));
  tx("ft", net.inBackground ? bi("Background", "后台") : "");
  if (ch) {
    const s = M.round((net.currentIntervalMs || 3e3) / 1e3);
    ch.disabled = !canTools; sa(ch, DON, en); sa(ch, DPS, paused); sa(ch, AP, en);
    ch.title = ts ? bi(`Sync ${fmtT(ts)}`, `同步 ${fmtT(ts)}`) : "";
    tx($("auto-lbl") || ch.lastElementChild, en ? bi(`Auto ${s}s`, `自动 ${s}s`) : bi("Auto off", "自动关"));
  }
  dis("lock-btn", locking || !canTools);
  dis("refresh-btn", app.isConnected() && !canTools); sa($("refresh-btn"), DBU, spin);
  vis("swp", first && cur);
  patchDot(); patchFb(); patchBnr();
}

function patchBright(pct, ok = true, showVal = ok) {
  tx("bv", showVal ? `${pct}%` : "-");
  const tr = btr();
  if (tr) { tr.style.setProperty("--pct", `${pct}%`); sa(tr, "aria-valuenow", pct); sa(tr, AD, !ok); tr.tabIndex = ok ? 0 : -1; }
  for (const b of qa(`[${DBS}],[${DBR}]`)) { b.disabled = !ok; const v = ga(b, DBR); if (v !== null) sa(b, AC, showVal && +v === pct); }
}

function patchEnv(d, prof, het, nice) {
  const el = $("de"); if (!el) return;
  const es = Object.entries(d.envVars || {}), qos = (prof === "utility" ? "Utility" : het ? "E-Core" : "Background") + " QoS";
  const k = `${lang}|${qos}|${nice}|${es.map((x) => x.join("=")).join(";")}`;
  if (k === envK) return; envK = k;
  const none = bi("No limits", "无限流");
  if (!el.replaceChildren || !doc.createElement) return tx(el, none);
  const ce = (t) => doc.createElement(t), mk = (pre, val) => { const s = ce("span"); s.className = "et"; if (pre) s.appendChild(doc.createTextNode(pre)); const b = ce("strong"); b.textContent = String(val); s.appendChild(b); return s; };
  if (!es.length) { const s = ce("span"); s.className = "cs"; s.textContent = none; return el.replaceChildren(s); }
  el.replaceChildren(mk("", qos), mk("nice ", nice), ...es.map(([x, v]) => mk(`${x}=`, v)));
}

const setBg = (id, t, k = "in", tip = "") => { const e = $(id); if (!e) return; vis(e, !!t); if (t) { sa(e, DK, k); tx(e, t); e.title = tip; } };

function patchAll(d) {
  if (!d) return; if (drag) { defr = true; return; }
  defr = false; vis("shell", 1); vis("emp", 0);

  const { policy: p = {}, settings: s = {}, display: disp = {}, host: h = {}, cpuTopology: ct = {}, lidState: lid, powerSource: pwr, batteryPct: bp0 } = d;
  const rm = h.memory || {}, ov = s.modeOverride || "auto", prof = p.profile || "normal", pL = mName(prof), closed = lid === "closed";
  const dispOk = hasD(d), bV = effB(d) ?? 20, rest = disp.savedPercent ?? bV;
  const [thL, thK] = thInfo(d), [grL, grK, grP] = grHero(d);

  patchDot(d);
  const bp = clampPercent(bp0), bk = pwr === "ac" ? "ok" : bp <= 20 ? "er" : bp <= 40 ? "wn" : "ok";
  for (const [id, k, t] of [["h-lid", lid === "open" ? "ok" : closed ? "ac" : "t3", lidS(lid)], ["h-pwr", bk, pwrS(pwr, bp0)], ["h-thm", thK, thL], ["h-grd", grK, grL]]) {
    sa($(id + "-d"), DK, k); tx(id, t);
  }
  vis("hbb", bp0 != null);
  const hbf = $("hbf"); if (hbf) { sa(hbf, DK, bk); hbf.style.width = `${bp}%`; }

  const asleep = Boolean(disp.confirmedAsleep ?? disp.asleep), rst = bi(`restores ${rest}%`, `恢复 ${rest}%`);
  setBg("bt", closed ? bi("Restore target", "恢复目标") : asleep ? asl() : "", closed ? "ac" : "in");
  patchBright(bV, canTools && dispOk, dispOk);
  const cap = !dispOk ? bi("No built-in display", "无内屏")
    : closed ? `${bi("Closed", "已合盖")} · ${disp.internalOffline ? bi("offline", "内屏离线") : asleep ? asl() : bi("online", "内屏在线")} · ${rst}`
    : asleep ? `${asl()} · ${rst}` : d.displayGuard?.thermalCapped ? bi("Thermal cap", "温控限亮中") : "";
  vis("bc", !!cap);

  const gOn = !gOff(d), gb = $("guard-btn");
  if (gb) { gb.disabled = !canTools; sa(gb, DON, gOn); sa(gb, DPS, grP); sa(gb, AC, gOn); }
  const rail = $("mode-rail");
  if (rail) {
    rail.style.setProperty("--idx", String(M.max(0, MODE_IDS.indexOf(ov))));
    for (const b of qa(`[${DM}]`)) { const sel = ga(b, DM) === ov; b.disabled = !canTools; sa(b, AC, sel); b.tabIndex = canTools && sel ? 0 : -1; }
  }
  setBg("mb", p.modeSource === "manual" ? bi("Manual", "手动") : prof !== "normal" ? pL : "", p.modeSource === "manual" ? "ac" : prof === "strict" ? "er" : "wn");
  const tot = num(ct.total, 8), jobs = num(p.maxJobs, tot), fi = describeFanState(d.fan, lang, grP);
  vis("ar", s.maxJobsOverride > 0);
  dis("ar", !canTools); dis("jd", !canTools || jobs <= 1); dis("ji", !canTools || jobs >= tot);
  setBg("fth", thL, thK); vis("frow2", fi.showController);
  if (fi.showController) { tx("fctrl", fi.controller); setBg("fown", fi.badgeText, fi.badgeKind, fi.tooltip); }
  const pC = num(ct.performanceCores), eC = num(ct.efficiencyCores), het = pC > 0;
  const topo = het ? `${pC}P+${eC}E` : bi(`${tot} cores`, `${tot} 核`), memP = clampPercent(rm.usedPct);
  const up = bi(`Up ${fmtUp(h.uptimeSeconds)}`, `运行 ${fmtUp(h.uptimeSeconds)}`);
  for (const [id, v] of [
    ["hs", `${lidS(lid)} · ${pwrS(pwr, bp0)} · ${pL}`], ["bc", cap], ["gs", describeGuardStatus(d, rest, lang)],
    ["sv", s.maxJobsOverride > 0 ? bi(`${jobs} ${jobs === 1 ? "job" : "jobs"}`, `${jobs} 并发`) : bi(`Auto · ${jobs}`, `自动 · ${jobs}`)],
    ["frpm", fi.rpmText], ["fcap", fi.caption], ["tp", `${topo} · ${bi(`Mem ${memP}%`, `内存 ${memP}%`)} · ${thL}`],
    ["dmu", num(rm.usedGb)], ["dmt", `/ ${num(rm.totalGb)} GB (${memP}%)`],
    ["dc", `${topo} · ${bi(`${num(rm.compressedGb)} GB comp.`, `压缩 ${num(rm.compressedGb)} GB`)}`],
    ["dld", `${bi("Load", "负载")} ${(h.loadAvg || []).join(" · ")}`],
    ["dsp", d.cpuSpeedLimitPct != null ? `CPU ${num(d.cpuSpeedLimitPct, 100)}% · ${up}` : up],
    ["ds", { no: bi("Awake on close", "合盖不休眠"), yes: bi("Sleeps on close", "合盖休眠") }[d.clamshellCausesSleep] || "—"],
    ["dlm", `${bi("Low Power:", "低电量:")} ${{ 1: bi("On", "开"), 0: bi("Off", "关") }[d.lowPowerMode] || "—"}`],
    ["dhi", `${h.hostname || "Mac"} · ${h.chip || "Mac"} · macOS ${h.macosVersion || ""}`],
  ]) tx(id, v);
  const dmf = $("dmf"); if (dmf) { sa(dmf, DK, memP >= 85 ? "er" : memP >= 60 ? "wn" : "ok"); dmf.style.width = `${memP}%`; }
  for (const b of qa(`[${DAC}]`)) { b.disabled = !canTools; sa(b, AC, ga(b, DAC) === (s.acClosedProfile || "utility")); }
  patchEnv(d, prof, het, p.niceLevel ?? 0);
  const dh = $("dh"); if (dh) dh.title = d.paths?.settingsPath || "";
  patchChrome();
}

async function rpc(name, args, track = true) {
  const t0 = performance.now(), res = await app.callServerTool({ name, arguments: args });
  if (res?.isError || !res?.structuredContent) throw new Error(res?.content?.[0]?.text || "err");
  const sc = res.structuredContent;
  if (track) { poller.recordExternalRpc({ totalMs: M.round(performance.now() - t0), serverMs: sc.serverMs ?? 0, markSuccess: true }); onOk(); }
  return sc;
}

const poller = createAdaptivePoller({
  defaultEnabled: false,
  shouldSkipBackgroundPoll: () => !canTools || syncing || bSync || drag || inF,
  onMetrics: (m) => { netM = m; patchChrome(); },
  fetcher: async ({ force, reason }) => {
    if (!canTools) return null;
    if (force) { loading = true; ct(busyT); busyT = st(() => loading && ((spin = true), patchChrome()), 150); }
    try {
      const sc = await rpc("clamshell_refresh", { force: !!force, ...(force ? { fresh: true } : {}), ...(hash ? { ifHash: hash } : {}) }, false);
      onOk(); applyData(sc);
      if (!sc.unchanged) patchAll(cur); else patchChrome();
      return sc;
    } catch (err) {
      onFail(reason);
      if (reason === "manual") fail("Refresh", "刷新", () => refreshNow(true));
      throw err;
    } finally { if (force) { ct(busyT); loading = spin = false; patchChrome(); } }
  },
});

const bCtrl = createBrightnessController({
  setter: (percent) => rpc("clamshell_set_brightness", { percent }),
  onOptimistic: (p) => { optB = p; patchBright(p, canTools, true); },
  onSyncChange: (s) => { bSync = s; patchFb(); },
  onCommit: (sc, target) => {
    optB = null; if (sc) applyData(sc);
    const a = sc?.brightnessAction, t = a?.mode === "saved_target", v = t ? a.savedPercent ?? target : a?.percent ?? target;
    patchAll(cur);
    t ? setOk(`Restores to ${v}%`, `开盖恢复 ${v}%`) : setOk(`Brightness ${v}%`, `亮度 ${v}%`);
  },
  onRollback: (_e, target) => {
    optB = null; const dispOk = hasD(cur);
    patchBright(effB(cur) ?? 20, canTools && dispOk, dispOk);
    fail("Brightness", "亮度", () => setB(target));
  },
});
const setB = (v, imm = true) => bCtrl.schedule(v, { immediate: imm });

const refreshNow = (force = true) => (canTools ? poller.trigger({ force, reason: force ? "manual" : "poll" }).catch(NOOP) : Promise.resolve());

async function lockNow() {
  if (locking || !canTools) return;
  locking = true; patchChrome();
  try {
    const r = await rpc("clamshell_lock_screen", {});
    r?.verified === false ? setOk("Lock requested", "已提交锁屏") : setOk("Locked", "已锁屏");
  } catch (e) { setFb("err", e?.message || bi("Lock failed", "锁屏失败"), lockNow); }
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
        if (ok) { patchAll(cur); setOk("Saved", "已保存"); }
      } catch {
        if (pend) { pend = { ...patch, ...pend }; continue; }
        cur = conf; patchAll(cur); fail("Save", "保存", () => queueCfg(patch)); break;
      }
    }
  } finally { inF = false; syncing = !!pend; patchFb(); if (pend) flushQ(); }
}

function queueCfg(p) { gen += 1; pend = { ...(pend || {}), ...p }; syncing = true; if (cur) { cur = computeOptimisticState(cur, p); patchAll(cur); } flushQ(); }

function syncCtx(ctx) {
  if (ctx?.locale && ctx.locale !== hostLoc) {
    hostLoc = ctx.locale;
    if (!storeLang()) {
      const n = resolvePanelLang({ saved: null, hostLocale: hostLoc, navLang: navL(), bootLocale: cur?.locale });
      if (n !== lang) { lang = n; patchI18n(); if (cur) patchAll(cur); }
    }
  }
}

const pctPtr = (tr, x) => { const r = tr.getBoundingClientRect(); return r.width ? clampPercent(((x - r.left) / r.width) * 100) : effB(cur) ?? 20; };

on(root, "click", (e) => {
  const b = cS(e, "button"); if (!b || b.disabled) return;
  const a = ga(b, DA);
  if (a === "refresh" || a === "bnr-retry") return app.isConnected() ? (canTools && refreshNow(true)) : connectHost(true);
  if (a === "bnr-dismiss") { bnrX = true; patchBnr(); return; }
  if (a === "retry") return fb?.retry?.();
  const lg = ga(b, DLG); if (lg && lg !== lang) { lang = lg; storeLang(lg); patchI18n(); if (cur) patchAll(cur); if (canTools) queueCfg({ lang: lg }); return; }
  if (!canTools) return;
  if (a === "lock") return lockNow();
  if (a === "auto") { autoWant = !poller.getMetrics().enabled; return poller.setEnabled(autoWant); }
  if (a === "guard") return queueCfg({ guardEnabled: gOff(cur) });
  if (a === "jauto") return queueCfg({ maxJobsOverride: 0 });
  const bs = ga(b, DBS); if (bs !== null) return setB(clampPercent((effB(cur) ?? 20) + +bs));
  const br = ga(b, DBR); if (br !== null) return setB(+br);
  const m = ga(b, DM); if (m) return queueCfg({ modeOverride: m });
  const js = ga(b, DJS); if (js !== null) { const t = num(cur?.cpuTopology?.total, 8); return queueCfg({ maxJobsOverride: M.max(1, M.min(t, num(cur?.policy?.maxJobs, t) + +js)) }); }
  const ac = ga(b, DAC); if (ac) queueCfg({ acClosedProfile: ac });
});

on(root, "pointerdown", (e) => {
  const tr = cS(e, "#bright-track"); if (!tr || ga(tr, AD) === "true") return;
  drag = true; sa(tr, DDR, "true"); tr.setPointerCapture?.(e.pointerId); setB(pctPtr(tr, e.clientX), false);
});
on(root, "pointermove", (e) => { const tr = drag && btr(); if (tr) setB(pctPtr(tr, e.clientX), false); });
function endDrag(e) {
  if (!drag) return; drag = false; const tr = btr(); tr?.removeAttribute(DDR);
  if (tr && e?.clientX != null) setB(pctPtr(tr, e.clientX)); else bCtrl.flush();
  if (defr && cur) patchAll(cur);
}
const cancelDrag = () => endDrag();
on(window, PU, endDrag); on(window, PC, cancelDrag);

on(root, "keydown", (e) => {
  if (e.key === "Escape") { const t = $("diag-tray"); if (t?.open) { t.open = false; t.querySelector("summary")?.focus(); e.preventDefault(); } return; }
  if (e.target?.id === "bright-track") {
    if (ga(e.target, AD) === "true") return;
    const c = effB(cur) ?? 20, s = e.shiftKey ? 10 : 1;
    const d = { [AR]: s, ArrowUp: s, [AL]: -s, ArrowDown: -s, PageUp: 10, PageDown: -10, Home: -c, End: 100 - c }[e.key];
    if (d !== undefined) { e.preventDefault(); setB(clampPercent(c + d), false); }
    return;
  }
  const dir = e.key === AR ? 1 : e.key === AL ? -1 : 0; if (!dir || !canTools) return;
  const mb = cS(e, `[${DM}]`), pb = cS(e, `[${DBR}]`);
  if (mb) { e.preventDefault(); const n = MODE_IDS[(MODE_IDS.indexOf(ga(mb, DM)) + dir + 5) % 5]; queueCfg({ modeOverride: n }); q1(`[${DM}="${n}"]`)?.focus(); }
  else if (pb) { e.preventDefault(); const n = PRESETS[(PRESETS.indexOf(+ga(pb, DBR)) + dir + 5) % 5]; setB(n); q1(`[${DBR}="${n}"]`)?.focus(); }
});

const detach = attachRevealRefresh({ doc, win: window, target: docEl, poller, onReveal: (r) => canTools && poller.forceNow(r).catch(NOOP) });
app.ontoolinput = () => { if (!cur) { loading = true; showEmpty(loadTxt()); } };
app.ontoolresult = (r) => {
  const sc = r?.structuredContent; if (!sc || typeof sc !== "object" || (!sc.unchanged && !sc.policy && !sc.host)) return;
  const prev = hash, had = !!cur; onOk(); applyData(sc); loading = false; if (canTools) poller.markInitialBoot();
  if (had && (sc.unchanged || (sc.hash && sc.hash === prev))) patchChrome(); else patchAll(cur);
};
app.onhostcontextchanged = (ctx) => { syncCtx(ctx); if (canTools && (ctx?.containerDimensions || ctx?.displayMode) && !drag && !bSync) poller.forceNow("host-context").catch(NOOP); };
app.onteardown = () => { ct(busyT); ct(fbT); detach(); poller.destroy(); bCtrl.destroy(); window.removeEventListener(PU, endDrag); window.removeEventListener(PC, cancelDrag); return {}; };

async function connectHost(manual = false) {
  try {
    await app.connect();
    canTools = app.hasServerTools();
    syncCtx(app.getHostContext());
    onOk();
    if (canTools) { if (autoWant) poller.setEnabled(true); if (cur) poller.markInitialBoot(); } else poller.setEnabled(false);
    redraw();
    if (canTools) {
      if (manual) await refreshNow(true);
      else if (boot?.stale) poller.trigger({ force: false, reason: "swr" }).catch(NOOP);
      else if (!cur) st(() => { if (!cur) refreshNow(true); }, 150);
    }
  } catch (e) {
    canTools = false; loading = first = false; poller.setEnabled(false);
    if (!cur) showEmpty(`${bi("Init failed: ", "初始化失败：")}${e?.message || e}`, true);
    else { bnrE = bi("Disconnected", "宿主断开"); bnrX = false; patchAll(cur); }
  }
}

(() => {
  mountControls(); patchI18n();
  if (boot) patchAll(boot); else showEmpty(loadTxt());
  connectHost(false);
})();
