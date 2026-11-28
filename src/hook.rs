use crate::i18n::{detect_lang, Lang};
use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use crate::policy::{
    compute_policy, format_profile_name, format_topology_compact, policy_settings_signature,
    state_signature,
};
use crate::state::{
    atomic_write, ensure_private_dir, read_bounded_regular_file, read_settings, settings_path,
    sha256_hex_16, MAX_STATE_FILE_BYTES,
};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_HOOK_STDIN_BYTES: usize = 64 * 1024;
const DEFAULT_POLL_TTL_SECS: u64 = 15;
const STALE_STATE_SECS: u64 = 7 * 86_400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInput {
    pub conversation_id: String,
    pub hook_event_name: String,
}

/// Early-exit balanced-JSON stdin reader: stops reading as soon as the root JSON object closes
/// (or at 64 KiB cap) so hooks finish in ~1–3 ms and never stall on open pipes.
pub fn parse_hook_stdin_early<R: Read>(mut reader: R) -> HookInput {
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    let mut started = false;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;

    while buf.len() < MAX_HOOK_STDIN_BYTES {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                let b = byte[0];
                if !started {
                    if b.is_ascii_whitespace() {
                        continue;
                    }
                    started = true;
                    if b != b'{' {
                        buf.push(b);
                        continue;
                    }
                }
                buf.push(b);
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        in_string = false;
                    }
                } else {
                    match b {
                        b'"' => in_string = true,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth <= 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(_) => break,
        }
    }

    let val: Value = serde_json::from_slice(&buf).unwrap_or_else(|_| json!({}));
    let conversation_id = val
        .get("conversationId")
        .and_then(|v| v.as_str())
        .map(|s| s.replace(['\r', '\n'], ""))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let hook_event_name = val
        .get("hookEventName")
        .and_then(|v| v.as_str())
        .map(|s| s.replace(['\r', '\n'], ""))
        .unwrap_or_default();

    HookInput {
        conversation_id,
        hook_event_name,
    }
}

fn sanitize_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    let trimmed = cleaned.trim_start_matches('.').replace("..", "_");
    if trimmed.is_empty() {
        "default".to_string()
    } else if trimmed.len() > 96 {
        format!("{}_{}", &trimmed[..64], sha256_hex_16(raw.as_bytes()))
    } else {
        trimmed
    }
}

fn mtime_secs(path: &Path) -> Option<u64> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return None;
    }
    meta.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn prune_stale_states(state_dir: &Path, now_secs: u64) {
    let Ok(entries) = fs::read_dir(state_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !(name_str.ends_with(".state") || name_str.contains(".state.tmp.")) {
            continue;
        }
        if let Some(mt) = mtime_secs(&path) {
            if now_secs.saturating_sub(mt) > STALE_STATE_SECS {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

pub fn evaluate_hook<P: ProbeBackend>(
    event_arg: Option<&str>,
    input: HookInput,
    data_dir: &Path,
    runner_bin: &str,
    probe: &P,
    ttl_secs: u64,
    now_secs: u64,
) -> Value {
    let event_name = if !input.hook_event_name.is_empty() {
        input.hook_event_name
    } else {
        match event_arg {
            Some("pre-invocation") | Some("PreInvocation") => "PreInvocation".to_string(),
            Some("session-start") | Some("SessionStart") => "SessionStart".to_string(),
            _ => String::new(),
        }
    };

    let safe_id = sanitize_id(&input.conversation_id);
    let state_dir = data_dir.join("state");
    if ensure_private_dir(&state_dir).is_err() {
        return json!({});
    }
    let state_file = state_dir.join(format!("{safe_id}.state"));
    let settings_file = settings_path(data_dir);

    let state_raw =
        read_bounded_regular_file(&state_file, MAX_STATE_FILE_BYTES).unwrap_or_default();
    let mut state_lines = state_raw.lines();
    let prev_sig = state_lines.next().unwrap_or("").trim().to_string();
    let prev_policy_settings_sig = state_lines
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut preloaded_settings = None;
    if event_name == "PreInvocation" && !prev_sig.is_empty() {
        let state_mt = mtime_secs(&state_file).unwrap_or(0);
        let age = now_secs.saturating_sub(state_mt);
        if age < ttl_secs {
            let settings_newer = mtime_secs(&settings_file)
                .map(|smt| smt >= state_mt)
                .unwrap_or(false);
            if !settings_newer {
                return json!({});
            }
            // Only policy-relevant settings (modeOverride, maxJobsOverride, acClosedProfile)
            // invalidate the PreInvocation TTL; toggling UI lang or guardEnabled must not bust it.
            let settings = read_settings(data_dir);
            let cur_policy_sig = policy_settings_signature(&settings);
            if prev_policy_settings_sig.as_deref() == Some(cur_policy_sig.as_str()) {
                return json!({});
            }
            preloaded_settings = Some(settings);
        }
    }

    if prev_sig.is_empty() {
        prune_stale_states(&state_dir, now_secs);
    }

    let host = probe.static_host();
    let hw = probe.probe_hardware();
    let settings = preloaded_settings.unwrap_or_else(|| read_settings(data_dir));
    let cur_policy_sig = policy_settings_signature(&settings);
    let lang = detect_lang(settings.lang.as_deref());
    let policy = compute_policy(
        &hw.lid_state,
        &hw.power_source,
        hw.battery_pct,
        &hw.thermal_state,
        &host.cpu_topology,
        &settings,
    );

    let sig = state_signature(&hw.lid_state, &hw.power_source, &hw.thermal_state, &policy);
    let _ = atomic_write(&state_file, format!("{sig}\n{cur_policy_sig}\n").as_bytes());

    if sig == prev_sig {
        return json!({});
    }
    let prev_was_normal = prev_sig.split('|').nth(3) == Some("normal");
    if (prev_sig.is_empty() || prev_was_normal) && policy.profile == "normal" {
        return json!({});
    }

    let prev_was_closed = prev_sig.split('|').next() == Some("closed");
    let batt_suffix = hw
        .battery_pct
        .map(|p| format!(" ({p}%)"))
        .unwrap_or_default();
    let sys_therm = match hw.cpu_speed_limit_pct {
        Some(pct) => format!(
            "{} (pressure={}, CPU_Speed_Limit={pct}%)",
            hw.thermal_state, hw.thermal_pressure
        ),
        None => format!("{} (pressure={})", hw.thermal_state, hw.thermal_pressure),
    };
    let topo_compact = format_topology_compact(&host.cpu_topology);
    let prof_name = format_profile_name(&policy.profile, lang);
    let jobs_en = if policy.max_jobs == 1 {
        "1 job".to_string()
    } else {
        format!("up to {} jobs", policy.max_jobs)
    };

    let (sys_msg, user_md) = if policy.profile == "normal" {
        let msg = format!(
            "[mac-clamshell] Live status update: lid={}, power={}{}, thermal={}, source={}. Throttling is now LIFTED (profile=normal, max_jobs={}). Normal execution speed and concurrency may resume.",
            hw.lid_state, hw.power_source, batt_suffix, sys_therm, policy.mode_source, policy.max_jobs
        );
        let md = match (
            lang,
            policy.mode_source.as_str(),
            prev_was_closed,
            hw.lid_state.as_str(),
        ) {
            (Lang::ZhCn, "manual", _, _) => {
                "> **Clam Guard** · 已手动切回**正常**模式。".to_string()
            }
            (Lang::En, "manual", _, _) => {
                "> **Clam Guard** · Switched to **Normal** (manual).".to_string()
            }
            (Lang::ZhCn, _, true, "open") => "> **Clam Guard** · 已开盖，恢复全速。".to_string(),
            (Lang::En, _, true, "open") => {
                "> **Clam Guard** · Lid open · back to full speed.".to_string()
            }
            (Lang::ZhCn, _, _, _) => "> **Clam Guard** · 已降温，恢复全速。".to_string(),
            (Lang::En, _, _, _) => {
                "> **Clam Guard** · Cooled down · back to full speed.".to_string()
            }
        };
        (msg, md)
    } else {
        let msg = format!(
            "[mac-clamshell] Live hardware status: lid={}, power={}{}, thermal={}, topology={}, source={}. Active profile={} (max_jobs={}, taskpolicy='{}', nice={}). Load the `mac-clamshell` skill and follow its rules: wrap CPU/I/O-heavy commands with `{runner_bin} run --sh \"<cmd>\"` (or `{runner_bin} run -- <cmd> ...`), cap parallelism/threads to {}, and avoid parallel heavy subagents.",
            hw.lid_state,
            hw.power_source,
            batt_suffix,
            sys_therm,
            topo_compact,
            policy.mode_source,
            policy.profile,
            policy.max_jobs,
            policy.taskpolicy,
            policy.nice_level,
            policy.max_jobs
        );
        let is_manual = policy.mode_source == "manual";
        let md = match (lang, hw.lid_state.as_str(), is_manual) {
            (Lang::ZhCn, "closed", true) => format!(
                "> **Clam Guard** · 已合盖 · 手动开启**{prof_name}**（最多 {} 个任务）。",
                policy.max_jobs
            ),
            (Lang::ZhCn, "closed", false) => format!(
                "> **Clam Guard** · 已合盖 · 已切到**{prof_name}**（最多 {} 个任务）。",
                policy.max_jobs
            ),
            (Lang::ZhCn, "open", true) => {
                format!("> **Clam Guard** · 已开盖 · 手动开启**{prof_name}**。")
            }
            (Lang::ZhCn, _, true) => format!(
                "> **Clam Guard** · 手动开启**{prof_name}**（最多 {} 个任务）。",
                policy.max_jobs
            ),
            (Lang::ZhCn, _, false) => {
                format!("> **Clam Guard** · 温度偏高，已切到**{prof_name}**。")
            }
            (Lang::En, "closed", true) => {
                format!("> **Clam Guard** · Lid closed · **{prof_name}** on (manual, {jobs_en}).")
            }
            (Lang::En, "closed", false) => {
                format!("> **Clam Guard** · Lid closed · **{prof_name}** on ({jobs_en}).")
            }
            (Lang::En, "open", true) => {
                format!("> **Clam Guard** · Lid open · **{prof_name}** on (manual).")
            }
            (Lang::En, _, true) => {
                format!("> **Clam Guard** · **{prof_name}** on (manual, {jobs_en}).")
            }
            (Lang::En, _, false) => {
                format!("> **Clam Guard** · Running hot · **{prof_name}** on.")
            }
        };
        (msg, md)
    };

    if event_name == "PreInvocation" {
        json!({
            "injectSteps": [{
                "systemMessage": {
                    "systemMessage": sys_msg,
                    "metadata": { "user_markdown": user_md }
                }
            }],
            "terminationBehavior": ""
        })
    } else {
        json!({
            "injectSteps": [{
                "systemMessage": {
                    "systemMessage": sys_msg,
                    "metadata": { "user_markdown": user_md }
                }
            }]
        })
    }
}

pub fn run_hook(event_arg: Option<&str>, data_dir: &Path) -> Value {
    if !cfg!(target_os = "macos") {
        return json!({});
    }
    let ttl_secs = std::env::var("MAC_CLAMSHELL_POLL_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_POLL_TTL_SECS);
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let runner_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "mac-clamshell-mcp".to_string());

    let input = parse_hook_stdin_early(io::stdin().lock());
    let probe = MacOsProbeBackend::default();
    evaluate_hook(
        event_arg,
        input,
        data_dir,
        &runner_bin,
        &probe,
        ttl_secs,
        now_secs,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::macos::{HardwareProbe, MemorySummary, StaticHostInfo};
    use crate::policy::CpuTopology;
    use crate::state::{write_settings, SettingsPatch};
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubProbe {
        lid: String,
        power: String,
        calls: AtomicUsize,
    }

    impl StubProbe {
        fn new(lid: &str, power: &str) -> Self {
            Self {
                lid: lid.into(),
                power: power.into(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl ProbeBackend for StubProbe {
        fn static_host(&self) -> StaticHostInfo {
            StaticHostInfo {
                hostname: "test-mac".into(),
                chip: "Apple M1 Pro".into(),
                macos_version: "15.4".into(),
                cpu_topology: CpuTopology {
                    total: 8,
                    performance_cores: 6,
                    efficiency_cores: 2,
                },
            }
        }
        fn lid_closed_fast(&self) -> Option<bool> {
            Some(self.lid == "closed")
        }
        fn probe_hardware(&self) -> HardwareProbe {
            self.calls.fetch_add(1, Ordering::SeqCst);
            HardwareProbe {
                lid_state: self.lid.clone(),
                clamshell_causes_sleep: "no".into(),
                power_source: self.power.clone(),
                battery_pct: Some(80),
                battery_detail: String::new(),
                thermal_state: "ok".into(),
                thermal_pressure: "nominal".into(),
                cpu_speed_limit_pct: None,
                low_power_mode: "0".into(),
                memory: MemorySummary {
                    total_gb: 16.0,
                    used_gb: 8.0,
                    compressed_gb: 1.0,
                    used_pct: 50,
                },
                load_avg: [1.0, 1.0, 1.0],
                uptime_seconds: 3600,
                degraded: false,
            }
        }
    }

    #[test]
    fn early_exit_stdin_parser_stops_at_balanced_brace() {
        let payload = br#"{"conversationId":"conv-42","nested":{"a":"}"},"hookEventName":"PreInvocation"}TRAILING_GARBAGE_NEVER_READ"#;
        let parsed = parse_hook_stdin_early(Cursor::new(&payload[..]));
        assert_eq!(parsed.conversation_id, "conv-42");
        assert_eq!(parsed.hook_event_name, "PreInvocation");

        assert_eq!(sanitize_id("../../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_id(".."), "default");
        assert_eq!(sanitize_id("...foo..bar"), "foo_bar");
        let long_id = "a".repeat(200);
        let sanitized = sanitize_id(&long_id);
        assert!(sanitized.len() <= 96);
    }

    #[test]
    fn hook_lifecycle_ttl_bilingual_and_no_fake_speed_limit() {
        let dir =
            std::env::temp_dir().join(format!("mac-clamshell-hook-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_settings(
            &dir,
            &SettingsPatch {
                lang: Some("en".into()),
                ..SettingsPatch::default()
            },
        )
        .unwrap();

        let closed = StubProbe::new("closed", "battery");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // 1. Fresh SessionStart under eco (English) emits injectSteps without fake CPU_Speed_Limit=100%
        let r1 = evaluate_hook(
            Some("session-start"),
            HookInput {
                conversation_id: "c1".into(),
                hook_event_name: "SessionStart".into(),
            },
            &dir,
            "mac-clamshell-mcp",
            &closed,
            60,
            now,
        );
        assert_eq!(closed.calls.load(Ordering::SeqCst), 1);
        assert_eq!(r1["injectSteps"].as_array().unwrap().len(), 1);
        assert!(r1.get("terminationBehavior").is_none());
        let sys1 = r1["injectSteps"][0]["systemMessage"]["systemMessage"]
            .as_str()
            .unwrap();
        let md1 = r1["injectSteps"][0]["systemMessage"]["metadata"]["user_markdown"]
            .as_str()
            .unwrap();
        assert!(sys1.contains("pressure=nominal"));
        assert!(!sys1.contains("CPU_Speed_Limit="));
        assert!(md1.contains("Lid closed"));

        // 2. Immediate PreInvocation within TTL hits cache -> {} (0 extra probes)
        let r2 = evaluate_hook(
            Some("pre-invocation"),
            HookInput {
                conversation_id: "c1".into(),
                hook_event_name: "PreInvocation".into(),
            },
            &dir,
            "mac-clamshell-mcp",
            &closed,
            60,
            now,
        );
        assert_eq!(r2, json!({}));
        assert_eq!(closed.calls.load(Ordering::SeqCst), 1);

        // 2b. Toggling non-policy settings (lang / guardEnabled / thermalBrightCapPct) MUST NOT
        // bust the PreInvocation TTL or trigger a hardware probe!
        write_settings(
            &dir,
            &SettingsPatch {
                lang: Some("zh-CN".into()),
                guard_enabled: Some(false),
                thermal_bright_cap_pct: Some(50),
                ..SettingsPatch::default()
            },
        )
        .unwrap();
        let r2b = evaluate_hook(
            Some("pre-invocation"),
            HookInput {
                conversation_id: "c1".into(),
                hook_event_name: "PreInvocation".into(),
            },
            &dir,
            "mac-clamshell-mcp",
            &closed,
            60,
            now,
        );
        assert_eq!(r2b, json!({}));
        assert_eq!(closed.calls.load(Ordering::SeqCst), 1);

        // 3. Same-second policy settings update to strict bypasses TTL and emits Chinese user_markdown
        write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("strict".into()),
                max_jobs_override: Some(1),
                lang: Some("zh-CN".into()),
                ..SettingsPatch::default()
            },
        )
        .unwrap();
        let r3 = evaluate_hook(
            Some("pre-invocation"),
            HookInput {
                conversation_id: "c1".into(),
                hook_event_name: "PreInvocation".into(),
            },
            &dir,
            "mac-clamshell-mcp",
            &closed,
            60,
            now,
        );
        assert_eq!(closed.calls.load(Ordering::SeqCst), 2);
        assert_eq!(r3["terminationBehavior"], "");
        assert!(r3["injectSteps"][0]["systemMessage"]["systemMessage"]
            .as_str()
            .unwrap()
            .contains("profile=strict"));
        assert!(
            r3["injectSteps"][0]["systemMessage"]["metadata"]["user_markdown"]
                .as_str()
                .unwrap()
                .contains("已合盖 · 手动开启**严格**")
        );

        // 4. Fresh SessionStart under normal emits {}
        let open = StubProbe::new("open", "ac");
        write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("auto".into()),
                max_jobs_override: Some(0),
                ..SettingsPatch::default()
            },
        )
        .unwrap();
        let r4 = evaluate_hook(
            Some("session-start"),
            HookInput {
                conversation_id: "c2".into(),
                hook_event_name: "SessionStart".into(),
            },
            &dir,
            "mac-clamshell-mcp",
            &open,
            60,
            now,
        );
        assert_eq!(r4, json!({}));

        // 5. Desktop (lid=unknown) & open-lid manual banners never say "合盖" / "Lid closed"
        let desktop_hot = StubProbe::new("unknown", "ac");
        write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("strict".into()),
                lang: Some("zh-CN".into()),
                ..SettingsPatch::default()
            },
        )
        .unwrap();
        let r_desk = evaluate_hook(
            Some("session-start"),
            HookInput {
                conversation_id: "c-desk".into(),
                hook_event_name: "SessionStart".into(),
            },
            &dir,
            "mac-clamshell-mcp",
            &desktop_hot,
            60,
            now,
        );
        let md_desk = r_desk["injectSteps"][0]["systemMessage"]["metadata"]["user_markdown"]
            .as_str()
            .unwrap();
        assert!(!md_desk.contains("合盖"));
        assert!(md_desk.contains("手动开启**严格**"));

        write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("eco".into()),
                lang: Some("en".into()),
                ..SettingsPatch::default()
            },
        )
        .unwrap();
        let r_open_man = evaluate_hook(
            Some("session-start"),
            HookInput {
                conversation_id: "c-open".into(),
                hook_event_name: "SessionStart".into(),
            },
            &dir,
            "mac-clamshell-mcp",
            &open,
            60,
            now,
        );
        let md_open = r_open_man["injectSteps"][0]["systemMessage"]["metadata"]["user_markdown"]
            .as_str()
            .unwrap();
        assert!(md_open.contains("Lid open · **Eco** on (manual)."));

        let _ = fs::remove_dir_all(&dir);
    }
}
