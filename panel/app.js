import { App, applyHostContext } from "./bridge.js";
import { createAdaptivePoller } from "./polling.js";
import { clampPercent, createBrightnessController } from "./brightness.js";

const $ = (id) => document.getElementById(id);
const root = $("app");
const tBoot = performance.now();
const bootData =
  typeof window !== "undefined" && window.__MAC_CLAMSHELL_BOOT__
    ? window.__MAC_CLAMSHELL_BOOT__
    : null;

const PRESETS = [0, 20, 50, 80, 100];
const MODES = [
  ["auto", "自动"],
  ["normal", "正常"],
  ["utility", "控温"],
  ["eco", "节能"],
  ["strict", "强限"],
];
const ENV_KEYS =
  "CARGO_BUILD_JOBS CMAKE_BUILD_PARALLEL_LEVEL OMP_NUM_THREADS OPENBLAS_NUM_THREADS MKL_NUM_THREADS VECLIB_MAXIMUM_THREADS RAYON_NUM_THREADS UV_THREADPOOL_SIZE GOMAXPROCS".split(
    " "
  );
const PROFILES = {
  eco: ["Eco 节能", "严格限制在 E-Core 运行"],
  utility: ["Utility 控温", "优先 E-Core 并抑制睿频"],
  strict: ["Strict 保护", "E-Core + 磁盘 I/O 强限流"],
  normal: ["Normal 正常", "无额外限流，全核释放"],
};

let curData = bootData;
let confirmedData = bootData;
let curHash = bootData?.hash || "";
let bootStale = Boolean(bootData?.stale);
let isLoading = !bootData;
let isSyncing = false;
let isBrightSyncing = false;
let optimisticBright = null;
let fbState = null;
let fbTimer = null;
let hostReadyMs = null;
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

const sa = (el, k, v) => el?.setAttribute(k, String(v));
const ga = (el, k) => el?.getAttribute(k);
const tx = (id, v) => {
  const el = $(id);
  if (el && el.textContent !== v) el.textContent = v;
};

function fmtUp(sec) {
  if (!Number.isFinite(sec) || sec <= 0) return "-";
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return d > 0 ? `${d}天 ${h}时` : h > 0 ? `${h}时 ${m}分` : `${m}分`;
}

function fmtTime(iso) {
  if (!iso) return "-";
  try {
    return new Date(iso).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return iso;
  }
}

const lidShort = (l) => (l === "closed" ? "合盖" : l === "open" ? "开盖" : "未知");
const powerShort = (p, pct) => {
  const s = pct != null ? ` ${pct}%` : "";
  return p === "battery" ? `电池${s}` : p === "ac" ? `插电${s}` : "未知电源";
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
    html = `<span class="fb p9 b1 g2 m2" data-k="sync"><span class="fd p9"></span><span>同步中…</span></span>`;
  } else if (fbState) {
    const r =
      fbState.kind === "err" && fbState.retry
        ? `<button type="button" class="rb" data-a="retry">重试</button>`
        : "";
    html = `<span class="fb p9 b1 g2 m2" data-k="${fbState.kind}"><span>${esc(fbState.text)}</span>${r}</span>`;
  }
  if (slot.innerHTML !== html) slot.innerHTML = html;
}

function optimistic(base, patch) {
  if (!base) return base;
  const s = {
    modeOverride: "auto",
    maxJobsOverride: 0,
    acClosedProfile: "utility",
    ...base.settings,
    ...patch,
  };
  const topo = base.cpuTopology || { total: 8, efficiencyCores: 2 };
  const autoProfile =
    base.thermalState === "warn"
      ? "strict"
      : base.lidState === "closed" && base.powerSource === "battery"
      ? "eco"
      : base.lidState === "closed"
      ? s.acClosedProfile || "utility"
      : "normal";

  const modeSource = s.modeOverride !== "auto" ? "manual" : "auto";
  let profile = modeSource === "manual" ? s.modeOverride : autoProfile;
  let maxJobs = topo.total || 8;
  let taskpolicy = "";
  let niceLevel = 0;

  if (profile === "strict") {
    maxJobs = 1;
    taskpolicy = "-b";
    niceLevel = 19;
  } else if (profile === "eco") {
    maxJobs = Math.min(2, topo.efficiencyCores || 2);
    if (modeSource === "auto" && base.batteryPct != null && base.batteryPct <= 20) maxJobs = 1;
    taskpolicy = "-c background";
    niceLevel = 15;
  } else if (profile === "utility") {
    maxJobs = Math.max(2, Math.min(4, topo.efficiencyCores || 2));
    taskpolicy = "-c utility";
    niceLevel = 10;
  } else {
    profile = "normal";
  }

  if (s.maxJobsOverride > 0) maxJobs = Math.min(s.maxJobsOverride, topo.total || 8);

  const envVars = {};
  if (profile !== "normal") {
    const j = String(maxJobs);
    envVars.MAKEFLAGS = `-j${j}`;
    for (const k of ENV_KEYS) envVars[k] = j;
  }

  return {
    ...base,
    settings: s,
    policy: { profile, autoProfile, modeSource, maxJobs, taskpolicy, niceLevel },
    envVars,
  };
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
  curData = isSyncing && curData?.settings ? optimistic(payload, curData.settings) : payload;
}

function fmtTel() {
  const m = netMetrics;
  const parts = ["Mac Clamshell v1.0.0"];
  if (m && m.lastNetRttMs !== null) {
    parts.push(`RTT ${m.lastNetRttMs}ms`, `采集 ${m.lastServerMs ?? 0}ms`);
    if (m.unchangedHits > 0) parts.push(`精简×${m.unchangedHits}`);
  } else if (hostReadyMs !== null) {
    parts.push(`握手 ${hostReadyMs}ms`);
  } else {
    parts.push("首屏直出");
  }
  if (m?.pausedHidden) parts.push("已暂停");
  return parts.join(" · ");
}

function patchChrome() {
  if (!mounted) return false;
  tx("ft", fmtTel());
  if (curData?.timestamp) tx("ht", fmtTime(curData.timestamp));

  const chip = $("auto-chip");
  if (chip) {
    const net = netMetrics || poller.getMetrics();
    const sec = Math.round((net.currentIntervalMs || 15000) / 1000);
    sa(chip, "data-on", net.enabled);
    sa(chip, "data-ps", net.enabled && net.pausedHidden);
    sa(chip, "aria-pressed", net.enabled);
    const lbl = net.enabled && net.pausedHidden ? "暂停" : `自动 ${sec}s`;
    if (chip.lastElementChild && chip.lastElementChild.textContent !== lbl) {
      chip.lastElementChild.textContent = lbl;
    }
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
  root.querySelectorAll("[data-bs]").forEach((b) => {
    b.disabled = !ok;
  });
  root.querySelectorAll("[data-br]").forEach((b) => {
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
  const s = d.settings || { modeOverride: "auto", maxJobsOverride: 0, acClosedProfile: "utility" };
  const [pLabel, pDesc] = PROFILES[p.profile] || PROFILES.normal;
  const disp = d.display || {};
  const isClosed = d.lidState === "closed";
  const bOk = Boolean(disp.available || isClosed);
  const bVal = effBright(d) ?? 20;

  sa($("hd"), "data-p", p.profile || "normal");
  tx("hs", `${lidShort(d.lidState)} · ${powerShort(d.powerSource, d.batteryPct)} · ${pLabel}`);

  const bTag = $("bt");
  if (bTag) {
    if (isClosed) {
      bTag.hidden = false;
      bTag.className = "bg p9 b1 g3 m2 ac";
      bTag.textContent = "恢复目标";
    } else if (disp.asleep) {
      bTag.hidden = false;
      bTag.className = "bg p9 b1 g3 m2";
      bTag.textContent = "已熄屏";
    } else {
      bTag.hidden = true;
    }
  }
  patchBright(bVal, bOk);
  tx(
    "bc",
    !bOk
      ? "未检测到内建显示屏"
      : isClosed
      ? "合盖中 · 调整值将在开盖后自动恢复"
      : disp.asleep
      ? "屏幕已熄屏 · 调整值将同步为唤醒目标"
      : "拖动或点按轨道实时调节 · 同步开盖恢复目标"
  );

  const idx = Math.max(0, MODES.findIndex(([id]) => id === s.modeOverride));
  const rail = $("mode-rail");
  if (rail) {
    rail.style.setProperty("--idx", String(idx));
    rail.querySelectorAll("[data-m]").forEach((b) => {
      const sel = ga(b, "data-m") === s.modeOverride;
      sa(b, "aria-checked", sel);
      b.tabIndex = sel ? 0 : -1;
    });
  }
  const badge = $("mb");
  if (badge) {
    badge.className = p.modeSource === "manual" ? "bg p9 b1 g3 m2 ac" : "bg p9 b1 g3 m2";
    badge.textContent = p.modeSource === "manual" ? "手动锁定" : "自动跟随";
  }

  const total = num(d.cpuTopology?.total, 8);
  const curJobs = num(p.maxJobs, total);
  tx("sv", s.maxJobsOverride > 0 ? `${curJobs} 核` : `自动 · ${curJobs} 核`);
  const rst = $("ar");
  if (rst) rst.hidden = !(s.maxJobsOverride > 0);
  const dec = $("jd");
  const inc = $("ji");
  if (dec) dec.disabled = curJobs <= 1;
  if (inc) inc.disabled = curJobs >= total;

  const sum = $("ps");
  if (sum) {
    const src = p.modeSource === "manual" ? "手动锁定" : "自动识别";
    const sch = p.taskpolicy
      ? `taskpolicy ${esc(p.taskpolicy)} · nice ${esc(p.niceLevel ?? 0)}`
      : "不施加调度限流";
    const nextSum = `<strong>${pLabel}</strong>（${src}）· ${pDesc} · <span class="mn">${sch} · max_jobs=${esc(curJobs)}</span>`;
    if (sum.innerHTML !== nextSum) sum.innerHTML = nextSum;
  }

  const pCores = num(d.cpuTopology?.performanceCores, 0);
  const eCores = num(d.cpuTopology?.efficiencyCores, 0);
  const rm = d.host?.memory || {};
  const memU = num(rm.usedGb, 0);
  const memT = num(rm.totalGb, 0);
  const memC = num(rm.compressedGb, 0);
  const memP = Math.max(0, Math.min(100, Math.round(num(rm.usedPct, 0))));
  const memLvl = memP >= 85 ? "danger" : memP >= 70 ? "warn" : "good";
  const battPct = Math.max(0, Math.min(100, num(d.batteryPct, 0)));
  const battLvl =
    d.powerSource === "ac" ? "good" : battPct <= 20 ? "danger" : battPct <= 40 ? "warn" : "good";

  for (const [id, val] of [
    ["tp", `${pCores}P+${eCores}E · 内存 ${memP}% · ${d.thermalState === "warn" ? "温控告警" : "温度正常"}`],
    ["dl", `${lidShort(d.lidState)}运行`],
    ["ds", `Sleep=${d.clamshellCausesSleep === "no" ? "已阻止" : d.clamshellCausesSleep ?? "?"}`],
    ["dg", isClosed ? `熄屏保护中 · 恢复 ${disp.savedPercent ?? bVal}%` : d.displayGuard ? "守护已就绪" : "按需运行"],
    ["dp", powerShort(d.powerSource, d.batteryPct)],
    ["dlm", `低电量：${d.lowPowerMode === "1" ? "已开" : d.lowPowerMode === "0" ? "未开" : "未知"}`],
    ["dt", d.thermalState === "warn" ? "温控告警" : "温度正常"],
    ["dsp", `CPU 上限 ${num(d.cpuSpeedLimitPct, 100)}% · ${fmtUp(d.host?.uptimeSeconds)}`],
    ["dld", `Load ${(d.host?.loadAvg || []).join(" / ")}`],
    ["dmu", String(memU)],
    ["dmt", `/ ${memT} GB (${memP}%)`],
    ["dc", `${pCores} P + ${eCores} E · 压缩 ${memC} GB`],
    ["dhi", `${d.host?.hostname || "Mac"} · ${d.host?.chip || "Apple Silicon"} · macOS ${d.host?.macosVersion || ""}`],
  ]) tx(id, val);

  const bBar = $("dbb");
  const bFill = $("dbf");
  if (bBar && bFill) {
    bBar.hidden = d.batteryPct == null;
    sa(bFill, "data-l", battLvl);
    bFill.style.width = `${battPct}%`;
  }
  const mFill = $("dmf");
  if (mFill) {
    sa(mFill, "data-l", memLvl);
    mFill.style.width = `${memP}%`;
  }

  root.querySelectorAll("[data-ac]").forEach((b) => {
    sa(b, "aria-checked", ga(b, "data-ac") === (s.acClosedProfile || "utility"));
  });

  const envEl = $("de");
  if (envEl) {
    const entries = Object.entries(d.envVars || {});
    const tag = (k, v) => `<span class="et p9 b2 g3 m2">${k}<strong>${esc(v)}</strong></span>`;
    const envHtml =
      entries.length > 0
        ? tag("taskpolicy ", p.taskpolicy) +
          tag("nice ", p.niceLevel) +
          entries.map(([k, v]) => tag(`${esc(k)}=`, v)).join("")
        : `<span class="cs m2">Normal 模式，当前不注入限流环境变量。</span>`;
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
  const cell = (k, body) => `<div class="c cl g2"><div class="ck">${k}</div>${body}</div>`;

  root.innerHTML = `<div id="shell" class="c shell">
<header class="r hb">
  <div class="f br">
    <div class="fc emb b1 g2 sh" aria-hidden="true"><svg width="18" height="18" viewBox="0 0 128 128" fill="none"><circle cx="64" cy="48" r="14" fill="#D97757" fill-opacity=".22"/><circle cx="64" cy="48" r="8.5" fill="#D97757"/><rect x="32" y="78" width="64" height="8" rx="4" fill="#D97757"/><rect x="24" y="90" width="80" height="9" rx="4.5" fill="currentColor" opacity=".88"/></svg></div>
    <div>
      <div class="ttl">Mac Clamshell</div>
      <div class="f sub m2"><span id="hd" class="bd p9" data-p="normal"></span><span id="hs"></span></div>
    </div>
  </div>
  <div class="f ha">
    <span id="fb" class="fb-slot"></span>
    ${btn("ch ns p9 b1 g2 sh m2", 'id="auto-chip" data-a="auto" data-on="false" data-ps="false" aria-pressed="false" aria-label="自动轮询"', '<span class="cd p9"></span><span>自动 15s</span>')}
    ${btn("ib p9 b1 g2 sh", 'id="refresh-btn" data-a="refresh" data-busy="false" aria-label="刷新"', '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round"><path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6"/></svg>')}
  </div>
</header>
<section class="c card warm b1 g2 sh">
  <div class="r">
    <div class="f lbl"><span>内建屏幕亮度</span><span id="bt" class="bg p9 b1 g3 m2" hidden></span></div>
    <span class="vb mn" id="bv">-</span>
  </div>
  <div class="f bs">
    ${btn("fc mb ns p9 b1 g3", 'data-bs="-5" aria-label="降5%"', "−")}
    <div id="bright-track" class="pt ns p9 b1 g3" role="slider" tabindex="0" aria-label="内建屏幕亮度" aria-valuemin="0" aria-valuemax="100" aria-valuenow="20" aria-disabled="false" data-z="false" style="--pct:20%">
      <div class="pf p9"></div>
      <div class="r pi"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M2 12h2m16 0h2M6.3 17.7l-1.4 1.4M19.1 4.9l-1.4 1.4"/></svg><span class="mn" id="biv">-</span></div>
    </div>
    ${btn("fc mb ns p9 b1 g3", 'data-bs="5" aria-label="升5%"', "+")}
  </div>
  <div class="prs" role="radiogroup" aria-label="亮度预设">${PRESETS.map((n) => btn("pb p9 b2 g3 m2", `role="radio" data-br="${n}" aria-checked="false"`, `${n}%`)).join("")}</div>
  <div id="bc" class="cap m2"></div>
</section>
<section class="c card b1 g2 sh">
  <div class="r">
    <div class="f lbl"><span>性能调度档位</span></div>
    <span id="mb" class="bg p9 b1 g3 m2">自动跟随</span>
  </div>
  <div id="mode-rail" class="sr p9 b2 g3" role="radiogroup" aria-label="性能档位" style="--idx:0">
    <div class="th p9 g2" aria-hidden="true"></div>
    ${MODES.map(([id, label], i) => btn("si p9 m2", `role="radio" data-m="${id}" aria-checked="${i === 0}" tabindex="${i === 0 ? 0 : -1}"`, label)).join("")}
  </div>
  <div class="r st-r">
    <div class="f sk"><span>并发上限</span>${btn("ar p9 b1 g3 m2", 'id="ar" data-a="jauto" hidden', "恢复自动")}</div>
    <div class="f sc p9 b2 g3" role="group" aria-label="并发上限">
      ${btn("fc sb p9 g2", 'id="jd" data-js="-1" aria-label="减1核"', "−")}
      <span id="sv" class="sv mn"></span>
      ${btn("fc sb p9 g2", 'id="ji" data-js="1" aria-label="加1核"', "+")}
    </div>
  </div>
  <div id="ps" class="sum b2 g3 m2"></div>
</section>
<details id="diag-tray" class="tray b1 g2 sh">
  <summary class="r ts ns">
    <span class="f sl"><span class="st-t">系统遥测与守护</span><span id="tp" class="spv el mn m2"></span></span>
    <svg class="cv-i" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="m9 18 6-6-6-6"/></svg>
  </summary>
  <div class="tb">
    <div class="stp">
      ${cell("盒盖与守护", '<div id="dl" class="cv"></div><div id="ds" class="cs m2 mn"></div><div id="dg" class="cs m2"></div>')}
      ${cell("电源与电池", '<div id="dp" class="cv"></div><div id="dbb" class="bar p9 g3"><div id="dbf" class="bf p9"></div></div><div id="dlm" class="cs m2"></div>')}
      ${cell("温控与负载", '<div id="dt" class="cv"></div><div id="dsp" class="cs m2 mn"></div><div id="dld" class="cs m2 mn"></div>')}
      ${cell("核心与内存", '<div class="cv"><span id="dmu"></span><span id="dmt" class="cu m2"></span></div><div class="bar p9 g3"><div id="dmf" class="bf p9"></div></div><div id="dc" class="cs m2"></div>')}
    </div>
    <div class="c tsec">
      <div class="r acr">
        <span>合盖插电默认档位</span>
        <div class="ss p9 b2 g3" role="radiogroup" aria-label="合盖插电档位">
          ${btn("ssb p9 m2", 'role="radio" data-ac="utility" aria-checked="true"', "Utility 控温")}
          ${btn("ssb p9 m2", 'role="radio" data-ac="eco" aria-checked="false"', "Eco 节能")}
        </div>
      </div>
      <div id="de" class="envs"></div>
    </div>
    <footer class="r colo">
      <span id="dh" class="mn cp el"><span id="dhi"></span> · <span id="ht"></span></span>
      <span class="mn" id="ft"></span>
    </footer>
  </div>
</details>
</div>`;
}

const poller = createAdaptivePoller({
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
      const res = await app.callServerTool({ name: "clamshell_refresh", arguments: args });
      const sc = res?.structuredContent;
      if (sc) {
        if (sc.stale === false) bootStale = false;
        applyTelemetry(sc);
        if (!sc.unchanged && sc.hash && sc.hash !== prevHash) patchAll(curData);
        else patchChrome();
      }
      return sc || {};
    } catch (err) {
      if (force) setFeedback("err", "刷新失败", () => refreshNow(true));
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
  setter: async (percent) => {
    const t0 = performance.now();
    const res = await app.callServerTool({
      name: "clamshell_set_brightness",
      arguments: { percent },
    });
    if (res?.isError) throw new Error(res.content?.[0]?.text || "亮度设置失败");
    const sc = res?.structuredContent;
    if (sc) {
      poller.recordExternalRpc({
        totalMs: Math.round(performance.now() - t0),
        serverMs: sc.serverMs ?? 0,
        markSuccess: true,
      });
    }
    return sc;
  },
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
    const fin = act?.mode === "saved_target" ? act.savedPercent ?? target : act?.percent ?? target;
    patchAll(curData);
    setFeedback("ok", act?.mode === "saved_target" ? `✓ 恢复目标 ${fin}%` : `✓ 已同步 ${fin}%`);
  },
  onRollback: (_err, target) => {
    optimisticBright = null;
    patchBright(effBright(curData) ?? 20, true);
    setFeedback("err", "亮度同步失败", () => brightnessCtrl.schedule(target, { immediate: true }));
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
    const t0 = performance.now();

    try {
      const res = await app.callServerTool({ name: "clamshell_configure", arguments: patch });
      const sc = res?.structuredContent;
      if (sc) {
        poller.recordExternalRpc({
          totalMs: Math.round(performance.now() - t0),
          serverMs: sc.serverMs ?? 0,
          markSuccess: true,
        });
        confirmedData = sc;
        if (sc.hash) curHash = sc.hash;
        curData =
          myGen === saveGen && !pendingPatch
            ? sc
            : curData?.settings
            ? optimistic(sc, curData.settings)
            : sc;
      }
      if (myGen === saveGen && !pendingPatch) {
        patchAll(curData);
        setFeedback("ok", "✓ 已同步");
      }
    } catch {
      if (!pendingPatch) {
        curData = confirmedData;
        patchAll(curData);
      }
      setFeedback("err", "保存失败", () => queueSettings(patch));
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
    curData = optimistic(curData, patch);
    patchAll(curData);
  }
  flushQueue();
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
  if (tr) {
    tr.removeAttribute("data-drag");
    if (e && typeof e.clientX === "number") {
      brightnessCtrl.schedule(pctFromPtr(tr, e.clientX), { immediate: true });
    } else {
      brightnessCtrl.flush();
    }
  }
  if (patchDeferred && curData) patchAll(curData);
}

function cancelDrag() {
  if (!isDraggingBright) return;
  isDraggingBright = false;
  $("bright-track")?.removeAttribute("data-drag");
  brightnessCtrl.flush();
  if (patchDeferred && curData) patchAll(curData);
}

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
    let next = null;
    if (e.key === "ArrowRight" || e.key === "ArrowUp") next = cur + step;
    else if (e.key === "ArrowLeft" || e.key === "ArrowDown") next = cur - step;
    else if (e.key === "PageUp") next = cur + 10;
    else if (e.key === "PageDown") next = cur - 10;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = 100;
    if (next !== null) {
      e.preventDefault();
      brightnessCtrl.schedule(clampPercent(next), { immediate: false });
    }
    return;
  }

  const mBtn = e.target?.closest?.("[data-m]");
  if (mBtn && (e.key === "ArrowRight" || e.key === "ArrowLeft")) {
    e.preventDefault();
    const i = MODES.findIndex(([id]) => id === ga(mBtn, "data-m"));
    const [nextId] = MODES[(i + (e.key === "ArrowRight" ? 1 : -1) + MODES.length) % MODES.length];
    queueSettings({ modeOverride: nextId });
    root.querySelector(`[data-m="${nextId}"]`)?.focus();
    return;
  }

  const pBtn = e.target?.closest?.("[data-br]");
  if (pBtn && (e.key === "ArrowRight" || e.key === "ArrowLeft")) {
    e.preventDefault();
    const i = PRESETS.indexOf(Number(ga(pBtn, "data-br")));
    const nextPct =
      PRESETS[(i + (e.key === "ArrowRight" ? 1 : -1) + PRESETS.length) % PRESETS.length];
    brightnessCtrl.schedule(nextPct, { immediate: true });
    root.querySelector(`[data-br="${nextPct}"]`)?.focus();
  }
});

const onVis = () => poller.handleVisibilityChange(document.hidden);
document.addEventListener("visibilitychange", onVis);

app.ontoolinput = () => {
  if (!curData && !mounted) {
    isLoading = true;
    root.innerHTML = `<div class="shell"><div class="emp m2">正在读取设备状态…</div></div>`;
  }
};

app.ontoolresult = (r) => {
  const sc = r?.structuredContent;
  if (!sc || typeof sc !== "object") return;
  const prevHash = curHash;
  const hadData = Boolean(curData);
  if (sc.stale === false) bootStale = false;
  applyTelemetry(sc);
  isLoading = false;
  poller.markInitialBoot();
  if (hadData && mounted && (sc.unchanged || (sc.hash && sc.hash === prevHash))) {
    patchChrome();
    return;
  }
  patchAll(curData);
};

app.onhostcontextchanged = applyHostContext;
app.onteardown = () => {
  if (busyTimer) clearTimeout(busyTimer);
  if (fbTimer) clearTimeout(fbTimer);
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
    root.innerHTML = `<div class="shell"><div class="emp m2">正在读取设备状态…</div></div>`;
  }
  try {
    await app.connect();
    hostReadyMs = Math.round(performance.now() - tBoot);
    applyHostContext(app.getHostContext());
    patchChrome();
    if (bootStale) {
      bootStale = false;
      poller.trigger({ force: false, reason: "swr" }).catch(() => {});
    } else if (!curData) {
      setTimeout(() => {
        if (!curData) refreshNow(true);
      }, 150);
    }
  } catch (err) {
    isLoading = false;
    if (!curData) {
      root.innerHTML = `<div class="shell"><div class="emp m2">初始化失败：${esc(err?.message || err)}</div></div>`;
    } else {
      setFeedback("err", "宿主连接异常", () => refreshNow(true));
    }
  }
}

init();
