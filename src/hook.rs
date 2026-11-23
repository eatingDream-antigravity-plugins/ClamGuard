use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use crate::policy::{
    compute_policy, format_lid_zh, format_power_zh, format_profile_zh, state_signature,
};
use crate::state::{atomic_write, ensure_private_dir, read_settings, settings_path, sha256_hex_16};
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
    fs::metadata(path)
        .ok()?
        .modified()
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

    let prev_sig = fs::read_to_string(&state_file)
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if event_name == "PreInvocation" && !prev_sig.is_empty() {
        let state_mt = mtime_secs(&state_file).unwrap_or(0);
        let settings_newer = mtime_secs(&settings_file)
            .map(|smt| smt >= state_mt)
            .unwrap_or(false);
        let age = now_secs.saturating_sub(state_mt);
        if !settings_newer && age < ttl_secs {
            return json!({});
        }
    }

    if prev_sig.is_empty() {
        prune_stale_states(&state_dir, now_secs);
    }

    let host = probe.static_host();
    let hw = probe.probe_hardware();
    let settings = read_settings(data_dir);
    let policy = compute_policy(
        &hw.lid_state,
        &hw.power_source,
        hw.battery_pct,
        &hw.thermal_state,
        &host.cpu_topology,
        &settings,
    );

    let sig = state_signature(&hw.lid_state, &hw.power_source, &hw.thermal_state, &policy);
    let _ = atomic_write(&state_file, format!("{sig}\n").as_bytes());

    if sig == prev_sig {
        return json!({});
    }
    if prev_sig.is_empty() && policy.profile == "normal" {
        return json!({});
    }

    let lid_zh = format_lid_zh(&hw.lid_state);
    let power_zh = format_power_zh(&hw.power_source, hw.battery_pct);
    let profile_zh = format_profile_zh(&policy);
    let src_zh = if policy.mode_source == "manual" {
        "手动锁定"
    } else {
        "自动识别"
    };
    let batt_suffix = hw
        .battery_pct
        .map(|p| format!(" ({p}%)"))
        .unwrap_or_default();

    let (sys_msg, user_md) = if policy.profile == "normal" {
        let msg = format!(
            "[mac-clamshell] Live status update: lid={}, power={}{}, thermal={}, source={}. Throttling is now LIFTED (profile=normal, max_jobs={}). Normal execution speed and concurrency may resume.",
            hw.lid_state, hw.power_source, batt_suffix, hw.thermal_state, policy.mode_source, policy.max_jobs
        );
        let md = if policy.mode_source == "manual" {
            format!(
                "> **Mac Clamshell**: 已**手动切换**至正常性能模式（当前{lid_zh}，{power_zh}，并发≤{}）。",
                policy.max_jobs
            )
        } else {
            format!("> **Mac Clamshell**: 检测到已**{lid_zh}**（{power_zh}，温度正常）— 已自动恢复正常性能释放。")
        };
        (msg, md)
    } else {
        let therm_zh = if hw.thermal_state == "warn" {
            format!("触发温控告警 (CPU_Speed_Limit={}%)", hw.cpu_speed_limit_pct)
        } else {
            "温度正常".to_string()
        };
        let msg = format!(
            "[mac-clamshell] Live hardware status: lid={}, power={}{}, thermal={} (CPU_Speed_Limit={}%), topology={}P+{}E, source={}. Active profile={} (max_jobs={}, taskpolicy='{}', nice={}). Load the `mac-clamshell` skill and follow its rules: wrap CPU/I/O-heavy commands with `{runner_bin} run --sh \"<cmd>\"` (or `{runner_bin} run -- <cmd> ...`), cap parallelism/threads to {}, and avoid parallel heavy subagents.",
            hw.lid_state,
            hw.power_source,
            batt_suffix,
            hw.thermal_state,
            hw.cpu_speed_limit_pct,
            host.cpu_topology.performance_cores,
            host.cpu_topology.efficiency_cores,
            policy.mode_source,
            policy.profile,
            policy.max_jobs,
            policy.taskpolicy,
            policy.nice_level,
            policy.max_jobs
        );
        let md = format!(
            "> **Mac Clamshell**（{src_zh}）: 当前**{lid_zh}运行**（{power_zh}，{therm_zh}）— 已启用 **{profile_zh}**。"
        );
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
    let probe = MacOsProbeBackend;
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

    struct StubProbe {
        lid: String,
        power: String,
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
            HardwareProbe {
                lid_state: self.lid.clone(),
                clamshell_causes_sleep: "no".into(),
                power_source: self.power.clone(),
                battery_pct: Some(80),
                battery_detail: String::new(),
                thermal_state: "ok".into(),
                cpu_speed_limit_pct: 100,
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
    fn hook_lifecycle_ttl_and_same_second_settings_invalidation() {
        let dir =
            std::env::temp_dir().join(format!("mac-clamshell-hook-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let closed = StubProbe {
            lid: "closed".into(),
            power: "battery".into(),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // 1. Fresh SessionStart under eco emits injectSteps without terminationBehavior
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
        assert_eq!(r1["injectSteps"].as_array().unwrap().len(), 1);
        assert!(r1.get("terminationBehavior").is_none());

        // 2. Immediate PreInvocation within TTL hits cache -> {}
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

        // 3. Same-second settings update bypasses TTL and emits PreInvocation with terminationBehavior=""
        write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("strict".into()),
                max_jobs_override: Some(1),
                ac_closed_profile: None,
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
        assert_eq!(r3["terminationBehavior"], "");
        assert!(r3["injectSteps"][0]["systemMessage"]["systemMessage"]
            .as_str()
            .unwrap()
            .contains("profile=strict"));

        // 4. Fresh SessionStart under normal emits {}
        let open = StubProbe {
            lid: "open".into(),
            power: "ac".into(),
        };
        write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("auto".into()),
                max_jobs_override: Some(0),
                ac_closed_profile: None,
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

        let _ = fs::remove_dir_all(&dir);
    }
}
