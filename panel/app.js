import { App, applyHostContext } from "./bridge.js";
import { createAdaptivePoller } from "./polling.js";
import { clampPercent, createBrightnessController } from "./brightness.js";
import { computeOptimisticState, describeGuardStatus, resolvePanelLang } from "./model.js";

const $ = (id) => document.getElementById(id);
const root = $("app");
const bootData =
  typeof window !== "undefined" && window.__MAC_CLAMSHELL_BOOT__
    ? window.__MAC_CLAMSHELL_BOOT__
    : null;

const PRESETS = [0, 20, 50, 80, 100];
const MODE_IDS = ["auto", "normal", "utility", "eco", "strict"];
const MODE_EN = { auto: "Auto", normal: "Normal", utility: "Utility", eco: "Eco", strict: "Strict" };
const MODE_ZH = { auto: "自动", normal: "正常", utility: "控温", eco: "节能", strict: "强限" };
const LANG_KEY = "mac-clamshell-lang";

const STATIC_TXT = [
  ["t-disp", "Built-in Display", "内建屏幕亮度"],
  ["t-guard", "Closed-lid screen guard", "合盖熄屏守护"],
  ["t-perf", "Performance Mode", "性能档位"],
  ["t-jobs", "Parallel Jobs", "并发上限"],
  ["ar", "Reset", "恢复自动"],
  ["t-tray", "System Details", "系统详情"],
  ["ck-lid", "Lid & Guard", "盒盖与守护"],
  ["ck-pwr", "Power & Battery", "电源与电池"],
  ["ck-thm", "Thermal & Load", "温控与负载"],
  ["ck-cpu", "Cores & Memory", "核心与内存"],
  ["t-ac", "Closed on AC power", "合盖插电默认档位"],
];

const STATIC_ARIA = [
  ["#auto-chip", "Auto-refresh", "自动轮询"],
  ["#refresh-btn", "Refresh", "刷新"],
  ["#lang-btn", "Switch language", "切换语言"],
  ["#jd", "1 fewer job", "减少 1 并发"],
  ["#ji", "1 more job", "增加 1 并发"],
  ['[data-bs="-5"]', "Decrease 5%", "降低 5%"],
  ['[data-bs="5"]', "Increase 5%", "升高 5%"],
];

const storeLang = (v) => {
  try {
    if (typeof localStorage !== "undefined") {
      return v === undefined ? localStorage.getItem(LANG_KEY) : localStorage.setItem(LANG_KEY, v);
    }
  } catch {}
  return null;
};

let hostLocale = null;
let curLang = resolvePanelLang({
  saved: storeLang(),
  hostLocale: null,
  navLang: typeof navigator !== "undefined" ? navigator.language : null,
  bootLocale: bootData?.locale,
});
document.documentElement.lang = curLang;

const isZh = () => curLang === "zh-CN";
const bi = (en, zh) => (isZh() ? zh : en);
const modeName = (id) => (isZh() ? MODE_ZH[id] : MODE_EN[id]) || id;

let curData = bootData;
let confirmedData = bootData;
let curHash = bootData?.hash || "";
let isLoading = !bootData;
let isSyncing = false;
let isBrightSyncing = false;
let optimisticBright = null;
let fbState = null;
let fbTimer = null;
let netMetrics = null;
let mounted = false;

let pendingPatch = null;
let saveInFlight = false;
let saveGen = 0;
let isDraggingBright = false;
let patchDeferred = false;
let busyTimer = null;
let showBusySpin = false;

const app = new App({ name: "Mac Clamshell", version: "1.0.0" }, {});

const esc = (s) =>
  String(s ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");

const num = (v, fb = 0) => {
  const n = Number(v);
  return Number.isFinite(n) ? n : fb;
};

const qa = (s) => root.querySelectorAll(s);
const sa = (el, k, v) => el?.setAttribute(k, String(v));
const ga = (el, k) => el?.getAttribute(k);
const tx = (el, v) => {
  const e = typeof el === "string" ? $(el) : el;
  if (e && e.textContent !== v) e.textContent = v;
};

function fmtUp(sec) {
  if (!Number.isFinite(sec) || sec <= 0) return "-";
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const [du, hu, mu] = isZh() ? ["天 ", "时 ", "分"] : ["d ", "h ", "m"];
  return d > 0 ? `${d}${du}${h}${hu.trim()}` : h > 0 ? `${h}${hu}${m}${mu}` : `${m}${mu}`;
}

function fmtTime(iso) {
  if (!iso) return "-";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toTimeString().slice(0, 8);
}

const lidShort = (l) =>
  ({ closed: bi("Closed", "已合盖"), open: bi("Open", "已开盖") }[l] || bi("Unknown", "未知"));

const powerShort = (p, pct) => {
  const s = pct != null ? ` ${pct}%` : "";
  const lbl = { battery: bi("Battery", "电池"), ac: bi("AC Power", "外接电源") }[p];
  return lbl ? lbl + s : bi("Unknown power", "未知电源");
};

const thermLabel = (d) => {
  const p = d?.thermalPressure;
  if (p === "critical") return bi("Critical", "严重过热");
  if (p === "serious" || d?.thermalState === "warn") return bi("Serious", "偏高降频");
  if (p === "fair") return bi("Fair", "轻度温升");
  return bi("Nominal", "温度正常");
};

function effBright(d) {
  if (optimisticBright !== null) return optimisticBright;
  const disp = d?.display;
  if (!disp) return null;
  return d.lidState === "closed" || disp.asleep
    ? disp.savedPercent ?? disp.percent ?? 20
    : disp.percent ?? disp.savedPercent ?? null;
}

function setFeedback(kind, text, retry = null) {
  fbState = { kind, text, retry };
  if (fbTimer) clearTimeout(fbTimer);
  if (kind === "ok") {
    fbTimer = setTimeout(() => {
      fbState = null;
      patchFeedback();
    }, 2400);
  }
  patchFeedback();
}

function patchFeedback() {
  const slot = $("fb");
  if (!slot) return;
  let html = "";
  if (isSyncing || isBrightSyncing) {
    html = `<span class="fb p9 b1 g2 m2" data-k="sync"><span class="fd p9"></span><span>${bi("Saving…", "同步中…")}</span></span>`;
  } else if (fbState) {
    const r =
      fbState.kind === "err" && fbState.retry
        ? `<button type="button" class="rb" data-a="retry">${bi("Retry", "重试")}</button>`
        : "";
    html = `<span class="fb p9 b1 g2 m2" data-k="${fbState.kind}"><span>${esc(fbState.text)}</span>${r}</span>`;
  }
  if (slot.innerHTML !== html) slot.innerHTML = html;
}

function applyTelemetry(payload) {
  if (!payload || typeof payload !== "object") return;
  if (payload.unchanged) {
    if (curData && payload.timestamp) {
      curData = { ...curData, timestamp: payload.timestamp };
      if (confirmedData) confirmedData = { ...confirmedData, timestamp: payload.timestamp };
    }
    if (payload.hash) curHash = payload.hash;
    return;
  }
  confirmedData = payload;
  if (payload.hash) curHash = payload.hash;
  curData =
    isSyncing && curData?.settings ? computeOptimisticState(payload, curData.settings) : payload;
}

function patchStaticI18n() {
  if (!mounted) return;
  document.documentElement.lang = curLang;
  for (const [id, en, zh] of STATIC_TXT) tx(id, bi(en, zh));
  for (const [sel, en, zh] of STATIC_ARIA) sa(root.querySelector(sel), "aria-label", bi(en, zh));
  qa("[data-m]").forEach((b) => tx(b, modeName(ga(b, "data-m"))));
  qa("[data-ac]").forEach((b) => tx(b, modeName(ga(b, "data-ac"))));
  qa("[data-lg]").forEach((el) => sa(el, "data-act", ga(el, "data-lg") === curLang));
}

function patchChrome() {
  if (!mounted) return false;
  if (curData?.timestamp) tx("ht", fmtTime(curData.timestamp));
  const net = netMetrics || poller.getMetrics();
  const pausedTxt = bi("Paused", "已暂停");
  tx("ft", net.pausedHidden ? pausedTxt : "");

  const chip = $("auto-chip");
  if (chip) {
    const sec = Math.round((net.currentIntervalMs || 15000) / 1000);
    sa(chip, "data-on", net.enabled);
    sa(chip, "data-ps", net.enabled && net.pausedHidden);
    sa(chip, "aria-pressed", net.enabled);
    tx(chip.lastElementChild, net.enabled && net.pausedHidden ? pausedTxt : bi(`Auto ${sec}s`, `自动 ${sec}s`));
  }

  sa($("refresh-btn"), "data-busy", showBusySpin);
  patchFeedback();
  return true;
}

function patchBright(pct, ok = true) {
  const v = ok ? `${pct}%` : "-";
  tx("bv", v);
  tx("biv", v);
  const tr = $("bright-track");
  if (tr) {
    tr.style.setProperty("--pct", `${pct}%`);
    sa(tr, "aria-valuenow", pct);
    sa(tr, "aria-disabled", !ok);
    sa(tr, "data-z", pct <= 0);
    tr.tabIndex = ok ? 0 : -1;
  }
  qa("[data-bs]").forEach((b) => {
    b.disabled = !ok;
  });
  qa("[data-br]").forEach((b) => {
    b.disabled = !ok;
    sa(b, "aria-checked", ok && Number(ga(b, "data-br")) === pct);
  });
}

function patchAll(d) {
  if (!d) return;
  if (isDraggingBright) {
    patchDeferred = true;
    return;
  }
  patchDeferred = false;
  if (!mounted) mountShell();

  const p = d.policy || {};
  const s = d.settings || {};
  const modeOv = s.modeOverride || "auto";
  const profKey = p.profile || "normal";
  const pLabel = modeName(profKey);
  const disp = d.display || {};
  const isClosed = d.lidState === "closed";
  const bOk = Boolean(disp.available || isClosed);
  const bVal = effBright(d) ?? 20;
  const restorePct = disp.savedPercent ?? bVal;

  sa($("hd"), "data-p", profKey);
  tx("hs", `${lidShort(d.lidState)} · ${powerShort(d.powerSource, d.batteryPct)} · ${pLabel}`);

  const setBadge = (id, txt, ac = false) => {
    const el = $(id);
    if (!el) return;
    el.hidden = !txt;
    if (txt) {
      el.className = ac ? "bg p9 b1 g3 m2 ac" : "bg p9 b1 g3 m2";
      tx(el, txt);
    }
  };
  setBadge("bt", isClosed ? bi("Restore target", "恢复目标") : disp.asleep ? bi("Asleep", "已熄屏") : "", isClosed);
  patchBright(bVal, bOk);

  const bc = $("bc");
  if (bc) {
    const capText = !bOk
      ? bi("No built-in display detected", "未检测到内建屏幕")
      : isClosed
      ? bi(`Closed · restores to ${restorePct}% on open`, `已合盖 · 开盖恢复 ${restorePct}%`)
      : disp.asleep
      ? bi(`Display asleep · restores to ${restorePct}% on wake`, `屏幕已熄屏 · 唤醒恢复 ${restorePct}%`)
      : d.displayGuard?.thermalCapped
      ? bi("Capped under thermal pressure", "温控护眼限亮中")
      : "";
    bc.hidden = !capText;
    tx(bc, capText);
  }

  const guardOn = s.guardEnabled !== false;
  const guardDesc = describeGuardStatus(d, restorePct, curLang);
  tx("gs", guardDesc);
  const gBtn = $("guard-btn");
  if (gBtn) {
    sa(gBtn, "data-on", guardOn);
    sa(gBtn, "aria-checked", guardOn);
    tx(gBtn.lastElementChild, guardOn ? bi("Active", "已开启") : bi("Paused", "已暂停"));
  }

  const idx = Math.max(0, MODE_IDS.indexOf(modeOv));
  const rail = $("mode-rail");
  if (rail) {
    rail.style.setProperty("--idx", String(idx));
    qa("[data-m]").forEach((b) => {
      const sel = ga(b, "data-m") === modeOv;
      sa(b, "aria-checked", sel);
      b.tabIndex = sel ? 0 : -1;
    });
  }

  setBadge("mb", p.modeSource === "manual" ? bi("Manual", "手动") : profKey !== "normal" ? pLabel : "", p.modeSource === "manual");

  const total = num(d.cpuTopology?.total, 8);
  const curJobs = num(p.maxJobs, total);
  tx(
    "sv",
    s.maxJobsOverride > 0
      ? bi(`${curJobs} ${curJobs === 1 ? "job" : "jobs"}`, `${curJobs} 并发`)
      : bi(`Auto · ${curJobs}`, `自动 · ${curJobs}`)
  );
  const rst = $("ar");
  if (rst) rst.hidden = !(s.maxJobsOverride > 0);
  const dec = $("jd");
  const inc = $("ji");
  if (dec) dec.disabled = curJobs <= 1;
  if (inc) inc.disabled = curJobs >= total;

  const pCores = num(d.cpuTopology?.performanceCores, 0);
  const eCores = num(d.cpuTopology?.efficiencyCores, 0);
  const hetero = pCores > 0;
  const topo = hetero ? `${pCores}P+${eCores}E` : bi(`${total} cores`, `${total} 核`);

  const h = d.host || {};
  const rm = h.memory || {};
  const memU = num(rm.usedGb, 0);
  const memT = num(rm.totalGb, 0);
  const memC = num(rm.compressedGb, 0);
  const memP = Math.max(0, Math.min(100, Math.round(num(rm.usedPct, 0))));
  const memLvl = memP >= 85 ? "danger" : memP >= 70 ? "warn" : "good";
  const battPct = Math.max(0, Math.min(100, num(d.batteryPct, 0)));
  const battLvl =
    d.powerSource === "ac" ? "good" : battPct <= 20 ? "danger" : battPct <= 40 ? "warn" : "good";

  const tText = thermLabel(d);
  const upStr = bi(`Up ${fmtUp(h.uptimeSeconds)}`, `运行 ${fmtUp(h.uptimeSeconds)}`);
  const spdStr =
    d.cpuSpeedLimitPct != null
      ? `${bi(`CPU limit ${num(d.cpuSpeedLimitPct, 100)}%`, `CPU 上限 ${num(d.cpuSpeedLimitPct, 100)}%`)} · ${upStr}`
      : upStr;

  for (const [id, val] of [
    ["tp", `${topo} · ${bi(`Mem ${memP}%`, `内存 ${memP}%`)} · ${tText}`],
    ["dl", lidShort(d.lidState)],
    [
      "ds",
      d.clamshellCausesSleep === "no"
        ? bi("Clamshell awake", "合盖不休眠")
        : d.clamshellCausesSleep === "yes"
        ? bi("Sleeps on close", "合盖休眠")
        : "—",
    ],
    ["dg", guardDesc],
    ["dp", powerShort(d.powerSource, d.batteryPct)],
    [
      "dlm",
      bi("Low Power: ", "低电量模式：") +
        (d.lowPowerMode === "1" ? bi("On", "已开") : d.lowPowerMode === "0" ? bi("Off", "未开") : "—"),
    ],
    ["dt", tText],
    ["dsp", spdStr],
    ["dld", bi(`Load ${(h.loadAvg || []).join(" · ")}`, `负载 ${(h.loadAvg || []).join(" · ")}`)],
    ["dmu", String(memU)],
    ["dmt", `/ ${memT} GB (${memP}%)`],
    ["dc", `${topo} · ${bi(`${memC} GB compressed`, `压缩 ${memC} GB`)}`],
    ["dhi", `${h.hostname || "Mac"} · ${h.chip || "Mac"} · macOS ${h.macosVersion || ""}`],
  ]) tx(id, val);

  const bBar = $("dbb");
  if (bBar) bBar.hidden = d.batteryPct == null;
  const setBar = (id, lvl, pct) => {
    const f = $(id);
    if (f) {
      sa(f, "data-l", lvl);
      f.style.width = `${pct}%`;
    }
  };
  setBar("dbf", battLvl, battPct);
  setBar("dmf", memLvl, memP);

  qa("[data-ac]").forEach((b) => sa(b, "aria-checked", ga(b, "data-ac") === (s.acClosedProfile || "utility")));

  const envEl = $("de");
  if (envEl) {
    const entries = Object.entries(d.envVars || {});
    const tag = (k, v) => `<span class="et p9 b2 g3 m2">${k}<strong>${esc(v)}</strong></span>`;
    const qosDesc =
      profKey === "utility"
        ? hetero
          ? bi("Utility QoS", "Utility 控温调度")
          : "Utility QoS + nice"
        : hetero
        ? bi("E-Core QoS + nice", "E-Core 节能调度")
        : "Background QoS + nice";
    const envHtml =
      entries.length > 0
        ? tag("", qosDesc) +
          tag("nice ", p.niceLevel) +
          entries.map(([k, v]) => tag(`${esc(k)}=`, v)).join("")
        : `<span class="cs">${bi("No active process limits.", "当前未启用限流参数。")}</span>`;
    if (envEl.innerHTML !== envHtml) envEl.innerHTML = envHtml;
  }

  const hostEl = $("dh");
  if (hostEl) hostEl.title = d.paths?.settingsPath || "";
  patchChrome();
}

function mountShell() {
  if (mounted) return;
  mounted = true;
  const btn = (cls, attrs, body) => `<button type="button" class="${cls}" ${attrs}>${body}</button>`;
  const cell = (kId, body) => `<div class="c cl g2"><div id="${kId}" class="ck"></div>${body}</div>`;

  root.innerHTML = `<div id="shell" class="c shell">
<header class="r hb">
  <div class="f br">
    <div class="fc emb b1 g2 sh" aria-hidden="true"><svg width="18" height="18" viewBox="0 0 32 32" fill="none"><circle cx="16" cy="12" r="3.5" fill="#D97757" fill-opacity=".22"/><circle cx="16" cy="12" r="2.1" fill="#D97757"/><rect x="8" y="19.5" width="16" height="2" rx="1" fill="#D97757"/><rect x="6" y="22.5" width="20" height="2.2" rx="1.1" fill="currentColor" opacity=".88"/></svg></div>
    <div>
      <div class="ttl">Mac Clamshell</div>
      <div class="f sub m2"><span id="hd" class="bd p9" data-p="normal"></span><span id="hs"></span></div>
    </div>
  </div>
  <div class="f ha">
    <span id="fb" class="fb-slot"></span>
    ${btn("ch lg ns p9 b1 g2 sh m2", 'id="lang-btn" data-a="lang"', '<span data-lg="en">EN</span><span>/</span><span data-lg="zh-CN">中文</span>')}
    ${btn("ch ns p9 b1 g2 sh m2", 'id="auto-chip" data-a="auto" aria-pressed="true"', `<span class="cd p9"></span><span>${bi("Auto 15s", "自动 15s")}</span>`)}
    ${btn("ib p9 b1 g2 sh", 'id="refresh-btn" data-a="refresh"', '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round"><path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6"/></svg>')}
  </div>
</header>
<section class="c card warm b1 g2 sh">
  <div class="r">
    <div class="f lbl"><span id="t-disp"></span><span id="bt" class="bg p9 b1 g3 m2" hidden></span></div>
    <span class="vb mn" id="bv">-</span>
  </div>
  <div class="f bs">
    ${btn("fc mb ns p9 b1 g3", 'data-bs="-5"', "−")}
    <div id="bright-track" class="pt ns p9 b1 g3" role="slider" aria-labelledby="t-disp" tabindex="0" aria-valuemin="0" aria-valuemax="100" aria-valuenow="20" style="--pct:20%">
      <div class="pf p9"></div>
      <div class="r pi"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M2 12h2m16 0h2M6.3 17.7l-1.4 1.4M19.1 4.9l-1.4 1.4"/></svg><span class="mn" id="biv">-</span></div>
    </div>
    ${btn("fc mb ns p9 b1 g3", 'data-bs="5"', "+")}
  </div>
  <div id="prs-grp" class="prs" role="radiogroup" aria-labelledby="t-disp">${PRESETS.map((n) => btn("pb p9 b2 g3 m2", `role="radio" data-br="${n}" aria-checked="false"`, `${n}%`)).join("")}</div>
  <div id="bc" class="cap m2" hidden></div>
  <div class="r gr">
    <div class="f gk"><span id="t-guard"></span><span id="gs" class="gs m2"></span></div>
    ${btn("ch ns p9 b1 g3 m2", 'id="guard-btn" role="switch" data-a="guard"', `<span class="cd p9"></span><span>${bi("Active", "已开启")}</span>`)}
  </div>
</section>
<section class="c card b1 g2 sh">
  <div class="r">
    <div class="f lbl"><span id="t-perf"></span></div>
    <span id="mb" class="bg p9 b1 g3 m2" hidden></span>
  </div>
  <div id="mode-rail" class="sr p9 b2 g3" role="radiogroup" aria-labelledby="t-perf" style="--idx:0">
    <div class="th p9 g2" aria-hidden="true"></div>
    ${MODE_IDS.map((id, idx) => btn("si p9 m2", `role="radio" data-m="${id}" aria-checked="${idx === 0}" tabindex="${idx === 0 ? 0 : -1}"`, "")).join("")}
  </div>
  <div class="r st-r">
    <div class="f sk"><span id="t-jobs"></span>${btn("ar p9 b1 g3 m2", 'id="ar" data-a="jauto" hidden', "")}</div>
    <div id="jobs-grp" class="f sc p9 b2 g3" role="group" aria-labelledby="t-jobs">
      ${btn("fc sb p9 g2", 'id="jd" data-js="-1"', "−")}
      <span id="sv" class="sv mn"></span>
      ${btn("fc sb p9 g2", 'id="ji" data-js="1"', "+")}
    </div>
  </div>
</section>
<details id="diag-tray" class="tray b1 g2 sh">
  <summary class="r ts ns">
    <span class="f sl"><span id="t-tray" class="st-t"></span><span id="tp" class="spv el mn m2"></span></span>
    <svg class="cv-i" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="m9 18 6-6-6-6"/></svg>
  </summary>
  <div class="tb">
    <div class="stp">
      ${cell("ck-lid", '<div id="dl" class="cv"></div><div id="ds" class="cs"></div><div id="dg" class="cs"></div>')}
      ${cell("ck-pwr", '<div id="dp" class="cv"></div><div id="dbb" class="bar p9 g3"><div id="dbf" class="bf p9"></div></div><div id="dlm" class="cs"></div>')}
      ${cell("ck-thm", '<div id="dt" class="cv"></div><div id="dsp" class="cs mn"></div><div id="dld" class="cs mn"></div>')}
      ${cell("ck-cpu", '<div class="cv"><span id="dmu"></span><span id="dmt" class="cu m2"></span></div><div class="bar p9 g3"><div id="dmf" class="bf p9"></div></div><div id="dc" class="cs"></div>')}
    </div>
    <div class="c tsec">
      <div class="r acr">
        <span id="t-ac"></span>
        <div id="ac-grp" class="ss p9 b2 g3" role="radiogroup" aria-labelledby="t-ac">
          ${btn("ssb p9 m2", 'role="radio" data-ac="utility" aria-checked="true"', "")}
          ${btn("ssb p9 m2", 'role="radio" data-ac="eco" aria-checked="false"', "")}
        </div>
      </div>
      <div id="de" class="envs"></div>
    </div>
    <footer class="r colo">
      <span id="dh" class="mn el"><span id="dhi"></span> · <span id="ht"></span></span>
      <span class="mn" id="ft"></span>
    </footer>
  </div>
</details>
</div>`;
  patchStaticI18n();
}

async function rpc(name, args, track = true) {
  const t0 = performance.now();
  const res = await app.callServerTool({ name, arguments: args });
  if (res?.isError || !res?.structuredContent) {
    throw new Error(res?.content?.[0]?.text || "err");
  }
  const sc = res.structuredContent;
  if (track) {
    poller.recordExternalRpc({
      totalMs: Math.round(performance.now() - t0),
      serverMs: sc.serverMs ?? 0,
      markSuccess: true,
    });
  }
  return sc;
}

const showEmpty = (msg) => {
  root.innerHTML = `<div class="shell"><div class="emp">${msg}</div></div>`;
};

const poller = createAdaptivePoller({
  defaultEnabled: true,
  shouldSkipBackgroundPoll: () =>
    isSyncing || isBrightSyncing || isDraggingBright || saveInFlight,
  onMetrics: (m) => {
    netMetrics = m;
    patchChrome();
  },
  fetcher: async ({ force }) => {
    if (force) {
      isLoading = true;
      clearTimeout(busyTimer);
      busyTimer = setTimeout(() => {
        if (isLoading) {
          showBusySpin = true;
          patchChrome();
        }
      }, 150);
    }
    const prevHash = curHash;
    try {
      const args = { force: Boolean(force) };
      if (curHash) args.ifHash = curHash;
      const sc = await rpc("clamshell_refresh", args, false);
      applyTelemetry(sc);
      if (!sc.unchanged && sc.hash && sc.hash !== prevHash) patchAll(curData);
      else patchChrome();
      return sc;
    } catch (err) {
      if (force) setFeedback("err", bi("Refresh failed", "刷新失败"), () => refreshNow(true));
      throw err;
    } finally {
      if (force) {
        clearTimeout(busyTimer);
        isLoading = false;
        showBusySpin = false;
        patchChrome();
      }
    }
  },
});

const brightnessCtrl = createBrightnessController({
  setter: (percent) => rpc("clamshell_set_brightness", { percent }),
  onOptimistic: (pct) => {
    optimisticBright = pct;
    patchBright(pct, true);
  },
  onSyncChange: (syncing) => {
    isBrightSyncing = syncing;
    patchFeedback();
  },
  onCommit: (sc, target) => {
    optimisticBright = null;
    if (sc) applyTelemetry(sc);
    const act = sc?.brightnessAction;
    const isTarget = act?.mode === "saved_target";
    const fin = isTarget ? act.savedPercent ?? target : act?.percent ?? target;
    patchAll(curData);
    setFeedback(
      "ok",
      isTarget ? bi(`✓ Restores to ${fin}%`, `✓ 开盖恢复 ${fin}%`) : bi(`✓ Brightness ${fin}%`, `✓ 亮度 ${fin}%`)
    );
  },
  onRollback: (_err, target) => {
    optimisticBright = null;
    const bOk = Boolean(curData?.display?.available || curData?.lidState === "closed");
    patchBright(effBright(curData) ?? 20, bOk);
    setFeedback("err", bi("Brightness update failed", "亮度调节失败"), () =>
      brightnessCtrl.schedule(target, { immediate: true })
    );
  },
});

const refreshNow = (force = true) =>
  poller.trigger({ force, reason: force ? "manual" : "poll" }).catch(() => {});

async function flushQueue() {
  if (saveInFlight || !pendingPatch) return;
  saveInFlight = true;
  isSyncing = true;
  patchFeedback();

  while (pendingPatch) {
    const patch = pendingPatch;
    pendingPatch = null;
    const myGen = saveGen;

    try {
      const sc = await rpc("clamshell_configure", patch);
      confirmedData = sc;
      if (sc.hash) curHash = sc.hash;
      const settled = myGen === saveGen && !pendingPatch;
      curData = settled
        ? sc
        : curData?.settings
        ? computeOptimisticState(sc, curData.settings)
        : sc;
      if (settled) {
        patchAll(curData);
        setFeedback("ok", bi("✓ Saved", "✓ 已保存"));
      }
    } catch {
      if (!pendingPatch) {
        curData = confirmedData;
        patchAll(curData);
      }
      setFeedback("err", bi("Save failed", "保存失败"), () => queueSettings(patch));
      break;
    }
  }

  saveInFlight = false;
  isSyncing = Boolean(pendingPatch);
  patchFeedback();
}

function queueSettings(patch) {
  saveGen += 1;
  pendingPatch = { ...(pendingPatch || {}), ...patch };
  isSyncing = true;
  if (curData) {
    curData = computeOptimisticState(curData, patch);
    patchAll(curData);
  }
  flushQueue();
}

function toggleLang() {
  curLang = isZh() ? "en" : "zh-CN";
  storeLang(curLang);
  patchStaticI18n();
  if (curData) patchAll(curData);
  queueSettings({ lang: curLang });
}

function syncHostContext(ctx) {
  applyHostContext(ctx);
  if (ctx?.locale && ctx.locale !== hostLocale) {
    hostLocale = ctx.locale;
    if (!storeLang()) {
      const next = resolvePanelLang({
        saved: null,
        hostLocale,
        navLang: typeof navigator !== "undefined" ? navigator.language : null,
        bootLocale: curData?.locale,
      });
      if (next !== curLang) {
        curLang = next;
        patchStaticI18n();
        if (curData) patchAll(curData);
      }
    }
  }
}

function pctFromPtr(track, x) {
  const r = track.getBoundingClientRect();
  return r.width ? clampPercent(((x - r.left) / r.width) * 100) : effBright(curData) ?? 20;
}

root.addEventListener("click", (e) => {
  const btn = e.target?.closest?.("button");
  if (!btn || btn.disabled) return;
  const a = ga(btn, "data-a");
  if (a === "refresh") return refreshNow(true);
  if (a === "auto") return poller.setEnabled(!poller.getMetrics().enabled);
  if (a === "lang") return toggleLang();
  if (a === "guard") return queueSettings({ guardEnabled: !(curData?.settings?.guardEnabled !== false) });
  if (a === "retry") return fbState?.retry?.();
  if (a === "jauto") return queueSettings({ maxJobsOverride: 0 });

  const bs = ga(btn, "data-bs");
  if (bs !== null) {
    return brightnessCtrl.schedule(clampPercent((effBright(curData) ?? 20) + Number(bs)), {
      immediate: true,
    });
  }
  const br = ga(btn, "data-br");
  if (br !== null) return brightnessCtrl.schedule(Number(br), { immediate: true });

  const m = ga(btn, "data-m");
  if (m) return queueSettings({ modeOverride: m });

  const js = ga(btn, "data-js");
  if (js !== null) {
    const total = num(curData?.cpuTopology?.total, 8);
    const next = Math.max(1, Math.min(total, num(curData?.policy?.maxJobs, total) + Number(js)));
    return queueSettings({ maxJobsOverride: next });
  }

  const ac = ga(btn, "data-ac");
  if (ac) queueSettings({ acClosedProfile: ac });
});

root.addEventListener("pointerdown", (e) => {
  const tr = e.target?.closest?.("#bright-track");
  if (!tr || ga(tr, "aria-disabled") === "true") return;
  isDraggingBright = true;
  sa(tr, "data-drag", "true");
  tr.setPointerCapture?.(e.pointerId);
  brightnessCtrl.schedule(pctFromPtr(tr, e.clientX), { immediate: false });
});

root.addEventListener("pointermove", (e) => {
  if (!isDraggingBright) return;
  const tr = $("bright-track");
  if (tr) brightnessCtrl.schedule(pctFromPtr(tr, e.clientX), { immediate: false });
});

function endDrag(e) {
  if (!isDraggingBright) return;
  isDraggingBright = false;
  const tr = $("bright-track");
  tr?.removeAttribute("data-drag");
  if (tr && e && typeof e.clientX === "number") {
    brightnessCtrl.schedule(pctFromPtr(tr, e.clientX), { immediate: true });
  } else {
    brightnessCtrl.flush();
  }
  if (patchDeferred && curData) patchAll(curData);
}

const cancelDrag = () => endDrag();

window.addEventListener("pointerup", endDrag);
window.addEventListener("pointercancel", cancelDrag);

root.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    const tray = $("diag-tray");
    if (tray?.open) {
      tray.open = false;
      tray.querySelector("summary")?.focus();
      e.preventDefault();
    }
    return;
  }

  if (e.target?.id === "bright-track") {
    if (ga(e.target, "aria-disabled") === "true") return;
    const cur = effBright(curData) ?? 20;
    const step = e.shiftKey ? 10 : 1;
    const delta = {
      ArrowRight: step,
      ArrowUp: step,
      ArrowLeft: -step,
      ArrowDown: -step,
      PageUp: 10,
      PageDown: -10,
      Home: -cur,
      End: 100 - cur,
    }[e.key];
    if (delta !== undefined) {
      e.preventDefault();
      brightnessCtrl.schedule(clampPercent(cur + delta), { immediate: false });
    }
    return;
  }

  const arrow = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
  if (!arrow) return;
  const mBtn = e.target?.closest?.("[data-m]");
  if (mBtn) {
    e.preventDefault();
    const nextId = MODE_IDS[(MODE_IDS.indexOf(ga(mBtn, "data-m")) + arrow + MODE_IDS.length) % MODE_IDS.length];
    queueSettings({ modeOverride: nextId });
    root.querySelector(`[data-m="${nextId}"]`)?.focus();
    return;
  }
  const pBtn = e.target?.closest?.("[data-br]");
  if (pBtn) {
    e.preventDefault();
    const nextPct = PRESETS[(PRESETS.indexOf(Number(ga(pBtn, "data-br"))) + arrow + PRESETS.length) % PRESETS.length];
    brightnessCtrl.schedule(nextPct, { immediate: true });
    root.querySelector(`[data-br="${nextPct}"]`)?.focus();
  }
});

const onVis = () => poller.handleVisibilityChange(document.hidden);
document.addEventListener("visibilitychange", onVis);

app.ontoolinput = () => {
  if (!curData && !mounted) {
    isLoading = true;
    showEmpty(bi("Reading Mac status…", "正在读取设备状态…"));
  }
};

app.ontoolresult = (r) => {
  const sc = r?.structuredContent;
  if (!sc || typeof sc !== "object") return;
  const prevHash = curHash;
  const hadData = Boolean(curData);
  applyTelemetry(sc);
  isLoading = false;
  poller.markInitialBoot();
  if (hadData && mounted && (sc.unchanged || (sc.hash && sc.hash === prevHash))) {
    patchChrome();
    return;
  }
  patchAll(curData);
};

app.onhostcontextchanged = syncHostContext;
app.onteardown = () => {
  clearTimeout(busyTimer);
  clearTimeout(fbTimer);
  poller.destroy();
  brightnessCtrl.destroy();
  document.removeEventListener("visibilitychange", onVis);
  window.removeEventListener("pointerup", endDrag);
  window.removeEventListener("pointercancel", cancelDrag);
  return {};
};

async function init() {
  if (bootData) {
    poller.markInitialBoot();
    patchAll(bootData);
  } else {
    showEmpty(bi("Reading Mac status…", "正在读取设备状态…"));
  }
  try {
    await app.connect();
    syncHostContext(app.getHostContext());
    patchChrome();
    if (bootData?.stale) {
      poller.trigger({ force: false, reason: "swr" }).catch(() => {});
    } else if (!curData) {
      setTimeout(() => {
        if (!curData) refreshNow(true);
      }, 150);
    }
  } catch (err) {
    isLoading = false;
    if (!curData) {
      showEmpty(`${bi("Initialization failed: ", "初始化失败：")}${esc(err?.message || err)}`);
    } else {
      setFeedback("err", bi("Host disconnected", "宿主连接异常"), () => refreshNow(true));
    }
  }
}

init();
