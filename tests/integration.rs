#[cfg(feature = "test-mocks")]
use serde_json::json;
use serde_json::Value;
use std::fs;
#[cfg(feature = "test-mocks")]
use std::io::{BufRead, BufReader, Write};
#[cfg(feature = "test-mocks")]
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
#[cfg(feature = "test-mocks")]
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "test-mocks")]
use std::time::{Duration, Instant};

static TEST_SEQ: AtomicU64 = AtomicU64::new(1);

fn temp_data_dir(tag: &str) -> PathBuf {
    let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("mac-clamshell-it-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_mac-clamshell-mcp")
}

#[cfg(feature = "test-mocks")]
const UI_EXTENSION: &str = "io.modelcontextprotocol/ui";
#[cfg(feature = "test-mocks")]
const UI_MIME: &str = "text/html;profile=mcp-app";

#[cfg(feature = "test-mocks")]
fn app_capabilities() -> Value {
    json!({"extensions": {UI_EXTENSION: {"mimeTypes": [UI_MIME]}}})
}

#[cfg(feature = "test-mocks")]
fn mock_command(dir: &std::path::Path, app: &str) -> Command {
    let mut command = Command::new(bin_path());
    command
        .args([app, "--data-dir"])
        .arg(dir)
        .env("MAC_CLAMSHELL_LAUNCHAGENTS_DIR", dir.join("agents"))
        .env("MAC_CLAMSHELL_SKIP_LAUNCHCTL", "1")
        .env("MAC_CLAMSHELL_MOCK_LID", "open")
        .env("MAC_CLAMSHELL_MOCK_POWER", "ac")
        .env("MAC_CLAMSHELL_MOCK_BATT", "88")
        .env("MAC_CLAMSHELL_MOCK_THERM", "ok")
        .env("MAC_CLAMSHELL_MOCK_DISPLAY", "builtin")
        .env("MAC_CLAMSHELL_MOCK_FAN", "fanless")
        .env("MAC_CLAMSHELL_MOCK_HOST", "1")
        .env("MAC_CLAMSHELL_LANG", "en");
    command
}

#[cfg(feature = "test-mocks")]
fn mcp_batch(mut command: Command, requests: &[(&str, Value)]) -> Vec<Value> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for (id, (method, params)) in requests.iter().enumerate() {
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            })
        )
        .unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses: Vec<Value> = BufReader::new(output.stdout.as_slice())
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    assert_eq!(responses.len(), requests.len());
    responses
}

#[cfg(feature = "test-mocks")]
#[test]
fn cli_status_json_and_brightness_closed_lid_safe_target() {
    let dir = temp_data_dir("status");

    let out = Command::new(bin_path())
        .args(["status", "--json", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_MOCK_LID", "closed")
        .env("MAC_CLAMSHELL_MOCK_POWER", "battery")
        .env("MAC_CLAMSHELL_MOCK_BATT", "18")
        .env("MAC_CLAMSHELL_MOCK_THERM", "ok")
        .env("MAC_CLAMSHELL_MOCK_BRIGHTNESS", "60")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["lidState"], "closed");
    assert_eq!(v["powerSource"], "battery");
    assert_eq!(v["batteryPct"], 18);
    assert_eq!(v["policy"]["profile"], "eco");
    assert_eq!(v["policy"]["maxJobs"], 1);
    assert_eq!(v["display"]["available"], true);
    assert_eq!(v["display"]["savedPercent"], 60);
    assert_eq!(v["hash"].as_str().unwrap().len(), 16);

    // Setting brightness with --lid-state closed MUST only persist target (never flashes live screen)
    let br = Command::new(bin_path())
        .args([
            "brightness",
            "set",
            "42",
            "--lid-state",
            "closed",
            "--json",
            "--data-dir",
        ])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(br.status.success());
    let bv: Value = serde_json::from_slice(&br.stdout).unwrap();
    assert_eq!(bv["mode"], "saved_target");
    assert_eq!(bv["deferred"], true);
    assert_eq!(bv["savedPercent"], 42);

    let saved: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("display-guard-state.json")).unwrap())
            .unwrap();
    assert_eq!(saved["saved_brightness"], 0.42);
    assert_eq!(saved["user_target"], true);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cli_run_wrapper_enforces_concurrency_vars_and_normal_fast_path() {
    let dir = temp_data_dir("run");

    let eco = Command::new(bin_path())
        .args([
            "run",
            "--mode",
            "eco",
            "--jobs",
            "2",
            "--quiet",
            "--data-dir",
        ])
        .arg(&dir)
        .args([
            "--sh",
            r#"printf "%s|%s|%s|%s" "$MAC_CLAMSHELL_PROFILE" "$MAC_CLAMSHELL_MAX_JOBS" "$MAKEFLAGS" "$GOMAXPROCS""#,
        ])
        .env("MAKEFLAGS", "-j2")
        .output()
        .unwrap();
    assert!(eco.status.success());
    assert_eq!(String::from_utf8_lossy(&eco.stdout), "eco|2|-j2|2");

    // Normal mode without --jobs skips CARGO_BUILD_JOBS
    let norm = Command::new(bin_path())
        .args(["run", "--mode", "normal", "--quiet", "--data-dir"])
        .arg(&dir)
        .args([
            "--sh",
            r#"printf "%s|%s" "$MAC_CLAMSHELL_PROFILE" "${CARGO_BUILD_JOBS:-unset}""#,
        ])
        .env_remove("CARGO_BUILD_JOBS")
        .output()
        .unwrap();
    assert!(norm.status.success());
    assert_eq!(String::from_utf8_lossy(&norm.stdout), "normal|unset");

    // Invalid --jobs returns exit 2
    let bad = Command::new(bin_path())
        .args(["run", "--jobs", "0", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(feature = "test-mocks")]
#[test]
fn cli_hook_and_mcp_stdio_end_to_end() {
    let dir = temp_data_dir("mcp");

    // 1. Hook SessionStart under mocked closed+battery emits valid injectSteps JSON
    let mut hook_child = Command::new(bin_path())
        .args(["hook", "session-start", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_MOCK_LID", "closed")
        .env("MAC_CLAMSHELL_MOCK_POWER", "battery")
        .env("MAC_CLAMSHELL_MOCK_BATT", "80")
        .env("MAC_CLAMSHELL_MOCK_THERM", "ok")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hook_child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"conversationId":"it-conv","hookEventName":"SessionStart"}"#)
        .unwrap();
    let hook_out = hook_child.wait_with_output().unwrap();
    assert!(hook_out.status.success());
    let hv: Value = serde_json::from_slice(&hook_out.stdout).unwrap();
    assert_eq!(hv["injectSteps"].as_array().unwrap().len(), 1);
    assert!(hv["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap()
        .contains("profile=eco"));

    // 2. Hook SessionStart under normal emits minimal {} (zero noise)
    let mut norm_hook = Command::new(bin_path())
        .args(["hook", "session-start", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_MOCK_LID", "open")
        .env("MAC_CLAMSHELL_MOCK_POWER", "ac")
        .env("MAC_CLAMSHELL_MOCK_BATT", "100")
        .env("MAC_CLAMSHELL_MOCK_THERM", "ok")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    norm_hook
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"conversationId":"it-normal","hookEventName":"SessionStart"}"#)
        .unwrap();
    let norm_out = norm_hook.wait_with_output().unwrap();
    assert!(norm_out.status.success());
    assert_eq!(String::from_utf8_lossy(&norm_out.stdout).trim(), "{}");

    // 3. Live MCP stdio server JSON-RPC handshake, SSR, dedup, brightness, configure
    let mut child = mock_command(&dir, "serve")
        .env("MAC_CLAMSHELL_MOCK_LID", "closed")
        .env("MAC_CLAMSHELL_MOCK_POWER", "battery")
        .env("MAC_CLAMSHELL_MOCK_FAN", "yield:Macs Fan Control:2400,2450")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let mut rpc = |id: u64, method: &str, params: Value| -> Value {
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(stdin, "{req}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    let init = rpc(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05", "capabilities": app_capabilities()
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "mac-clamshell-mcp");
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(
        init["result"]["capabilities"]["extensions"][UI_EXTENSION]["mimeTypes"],
        json!([UI_MIME])
    );

    let tools = rpc(2, "tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 5);

    let res = rpc(
        3,
        "resources/read",
        json!({"uri": "ui://mac-clamshell-mcp/panel"}),
    );
    let html = res["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(html.contains("window.__MAC_CLAMSHELL_BOOT__="));
    assert!(!html.contains("/*__MAC_CLAMSHELL_BOOT__*/"));

    let panel = rpc(
        4,
        "tools/call",
        json!({"name": "clamshell_panel", "arguments": {}}),
    );
    let fan = &panel["result"]["structuredContent"]["fan"];
    assert_eq!(fan["controller"], "Macs Fan Control");
    assert_eq!(fan["ownershipMode"], "yield");
    assert_eq!(fan["fans"][0]["rpm"], 2400);
    assert!(panel["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("read-only, SMC manual, detected fan control: Macs Fan Control"));
    let hash = panel["result"]["structuredContent"]["hash"]
        .as_str()
        .unwrap()
        .to_string();

    let dedup = rpc(
        5,
        "tools/call",
        json!({"name": "clamshell_refresh", "arguments": {"ifHash": hash, "force": false}}),
    );
    assert_eq!(dedup["result"]["structuredContent"]["unchanged"], true);

    let br = rpc(
        6,
        "tools/call",
        json!({"name": "clamshell_set_brightness", "arguments": {"percent": 55}}),
    );
    assert_eq!(
        br["result"]["structuredContent"]["brightnessAction"]["mode"],
        "saved_target"
    );
    assert_eq!(
        br["result"]["structuredContent"]["brightnessAction"]["savedPercent"],
        55
    );

    let cfg = rpc(
        7,
        "tools/call",
        json!({"name": "clamshell_configure", "arguments": {"modeOverride": "utility"}}),
    );
    assert_eq!(
        cfg["result"]["structuredContent"]["policy"]["profile"],
        "utility"
    );

    let cfg_hash = cfg["result"]["structuredContent"]["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let fresh_dedup = rpc(
        8,
        "tools/call",
        json!({"name": "clamshell_refresh", "arguments": {"ifHash": cfg_hash, "fresh": true}}),
    );
    assert_eq!(
        fresh_dedup["result"]["structuredContent"]["unchanged"],
        true
    );
    assert_eq!(
        fresh_dedup["result"]["structuredContent"]["fromCache"],
        false
    );

    let lock_res = rpc(
        9,
        "tools/call",
        json!({"name": "clamshell_lock_screen", "arguments": {}}),
    );
    assert!(lock_res["result"].get("isError").is_none());
    assert_eq!(lock_res["result"]["structuredContent"]["ok"], true);
    assert_eq!(lock_res["result"]["structuredContent"]["method"], "mock");
    assert!(
        lock_res["result"]["structuredContent"]["locked_at_ms"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );

    drop(stdin);
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(feature = "test-mocks")]
#[test]
fn cli_lock_json_mock_and_error_path() {
    let dir = temp_data_dir("lock");

    let ok_out = Command::new(bin_path())
        .args(["lock", "--json", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_MOCK_DISPLAY", "builtin")
        .output()
        .unwrap();
    assert!(
        ok_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ok_out.stderr)
    );
    let ok_v: Value = serde_json::from_slice(&ok_out.stdout).unwrap();
    assert_eq!(ok_v["ok"], true);
    assert_eq!(ok_v["method"], "mock");
    assert!(ok_v["locked_at_ms"].as_u64().unwrap_or(0) > 0);

    let err_out = Command::new(bin_path())
        .args(["lock", "--json", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_MOCK_DISPLAY", "fail_lock")
        .output()
        .unwrap();
    assert_eq!(err_out.status.code(), Some(1));
    let err_v: Value = serde_json::from_slice(&err_out.stdout).unwrap();
    assert_eq!(err_v["ok"], false);
    assert!(err_v["error"]
        .as_str()
        .unwrap()
        .contains("mock lock screen failed"));

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(feature = "test-mocks")]
#[test]
fn mcp_plain_host_gets_supported_version_and_public_tools_only() {
    let dir = temp_data_dir("mcp-plain");
    let responses = mcp_batch(
        mock_command(&dir, "serve"),
        &[
            (
                "initialize",
                json!({"protocolVersion": "2099-01-01", "capabilities": {}}),
            ),
            ("tools/list", json!({})),
        ],
    );
    let initial = &responses[0]["result"];
    assert_eq!(initial["protocolVersion"], "2025-11-25");
    assert!(initial["capabilities"].get("extensions").is_none());
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "clamshell_panel",
            "clamshell_set_brightness",
            "clamshell_lock_screen"
        ]
    );
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn mcp_and_cli_require_verified_session_lock_after_display_sleep() {
    let dir = temp_data_dir("pmset-fallback");
    for (mode, locked) in [("pmset_locked", true), ("pmset_unconfirmed", false)] {
        let out = mock_command(&dir, "lock")
            .arg("--json")
            .env("MAC_CLAMSHELL_MOCK_DISPLAY", mode)
            .output()
            .unwrap();
        assert_eq!(out.status.success(), locked);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], locked);

        let mut command = mock_command(&dir, "serve");
        command.env("MAC_CLAMSHELL_MOCK_DISPLAY", mode);
        let responses = mcp_batch(
            command,
            &[
                ("initialize", json!({"protocolVersion": "2025-11-25"})),
                ("tools/call", json!({"name": "clamshell_lock_screen"})),
            ],
        );
        let res = &responses[1]["result"];
        if locked {
            assert_eq!(v["method"], "pmset");
            assert!(v["locked_at_ms"].as_u64().unwrap() > 0);
            assert!(res.get("isError").is_none());
            assert_eq!(res["structuredContent"]["method"], "pmset");
            assert!(res["structuredContent"]["locked_at_ms"].as_u64().unwrap() > 0);
        } else {
            let message = "display to sleep without a verified macOS session lock";
            assert!(v["error"].as_str().unwrap().contains(message));
            assert!(v.get("locked_at_ms").is_none());
            assert_eq!(res["isError"], true);
            assert_eq!(res["structuredContent"]["errorKind"], "lock_failed");
            assert!(res["structuredContent"]["errorDetail"]
                .as_str()
                .unwrap()
                .contains(message));
            assert_eq!(res["content"][0]["text"], "Couldn't lock the screen.");
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn cli_brightness_defers_unknown_lid_and_unconfirmed_awake_state() {
    let dir = temp_data_dir("brightness-unknown");
    for (lid, display, target) in [
        ("unknown", "builtin", "31"),
        ("open", "unknown_sleep", "32"),
        ("open", "asleep", "33"),
    ] {
        let out = mock_command(&dir, "brightness")
            .args(["set", target, "--json"])
            .env("MAC_CLAMSHELL_MOCK_LID", lid)
            .env("MAC_CLAMSHELL_MOCK_DISPLAY", display)
            .env("MAC_CLAMSHELL_MOCK_BRIGHTNESS", "70")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["mode"], "saved_target");
        assert_eq!(v["applied"], false);
        assert_eq!(v["deferred"], true);
        assert_eq!(v["percent"], 70);
        let state: Value =
            serde_json::from_slice(&fs::read(dir.join("display-guard-state.json")).unwrap())
                .unwrap();
        assert_eq!(
            state["saved_brightness"],
            target.parse::<f64>().unwrap() / 100.0
        );
        assert_eq!(state["user_target"], true);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn cli_daemon_refuses_when_another_process_holds_the_guard_lock() {
    let dir = temp_data_dir("daemon-contention");
    let lock = fs::File::create(dir.join("display-guard-daemon.lock")).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let mut child = mock_command(&dir, "watch")
        .args(["--daemon", "--interval", "2"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("watch should exit without entering the guard loop");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("another display-guard daemon already holds the lock"));
    assert!(!dir.join("display-guard-state.json").exists());
    drop(lock);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn cli_guard_uninstalls_fixture_without_mutating_live_launchd() {
    let dir = temp_data_dir("guard-uninstall");
    let agents = dir.join("agents");
    fs::create_dir(&agents).unwrap();
    let plist = agents.join("dev.mac-clamshell-mcp.display-guard.plist");
    fs::write(&plist, "<plist/>").unwrap();
    for _ in 0..2 {
        let out = mock_command(&dir, "guard")
            .args(["uninstall", "--json"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(!plist.exists());
        let status: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(status["loaded"], false);
        assert_eq!(status["plistInstalled"], false);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn concurrent_mcp_settings_updates_survive_across_processes() {
    let dir = temp_data_dir("settings");
    let patches = [
        json!({"modeOverride": "strict"}),
        json!({"guardEnabled": false}),
        json!({"lang": "zh-CN"}),
    ];
    let barrier = std::sync::Barrier::new(patches.len());
    std::thread::scope(|scope| {
        for patch in patches {
            let command = mock_command(&dir, "serve");
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                let responses = mcp_batch(command, &[
                    ("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": app_capabilities()})),
                    ("tools/call", json!({"name": "clamshell_configure", "arguments": patch})),
                ]);
                assert!(responses[1]["result"].get("isError").is_none());
            });
        }
    });
    let out = mock_command(&dir, "status").arg("--json").output().unwrap();
    assert!(out.status.success());
    let state: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(state["settings"]["modeOverride"], "strict");
    assert_eq!(state["settings"]["guardEnabled"], false);
    assert_eq!(state["settings"]["lang"], "zh-CN");
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn fan_samples_are_read_only_in_cli_and_mcp_with_bilingual_text() {
    let dir = temp_data_dir("fans");
    for (mock, status, smc_mode, controller, ownership, expected_rpms) in [
        (
            "fanless",
            "fanless",
            "none",
            "System (macOS)",
            "observer",
            vec![],
        ),
        (
            "active:2140,2210",
            "ok",
            "auto",
            "System (macOS)",
            "observer",
            vec![2140, 2210],
        ),
        (
            "yield:Macs Fan Control:2400,2450",
            "ok",
            "manual",
            "Macs Fan Control",
            "yield",
            vec![2400, 2450],
        ),
        (
            "manual:2100",
            "ok",
            "manual",
            "Manual (SMC)",
            "yield",
            vec![2100],
        ),
        (
            "unknown_mode:2100,2200",
            "ok",
            "unknown",
            "Unknown",
            "observer",
            vec![2100, 2200],
        ),
        (
            "monitor:Fanny:2100",
            "ok",
            "auto",
            "System (macOS)",
            "observer",
            vec![2100],
        ),
        (
            "unavailable",
            "unavailable",
            "unknown",
            "Unknown",
            "observer",
            vec![],
        ),
    ] {
        let check = |fan: &Value| {
            assert_eq!(fan["status"], status);
            assert_eq!(fan["smcMode"], smc_mode);
            assert_eq!(fan["controller"], controller);
            assert_eq!(fan["ownershipMode"], ownership);
            assert_eq!(fan["readOnly"], true);
            assert_eq!(fan["stale"], false);
            assert!(fan["sampledAtMs"].as_u64().unwrap() > 0);
            assert!(fan["note"].as_str().unwrap().contains("read-only"));
            let fans = fan["fans"].as_array().unwrap();
            assert_eq!(
                fans.iter()
                    .map(|f| f["rpm"].as_i64().unwrap())
                    .collect::<Vec<_>>(),
                expected_rpms
            );
            if fans.len() == 2 {
                assert_eq!(fans[0]["label"], "Left");
                assert_eq!(fans[1]["label"], "Right");
            } else if mock.starts_with("manual:") {
                assert_eq!(fans[0]["label"], "Fan 1");
                assert_eq!(fans[0]["manual"], true);
            }
            if mock.starts_with("yield:") {
                assert_eq!(fan["externalManagers"], json!(["Macs Fan Control"]));
            } else {
                assert_eq!(fan["externalManagers"], json!([]));
            }
            if mock.starts_with("monitor:") {
                assert_eq!(fan["detectedMonitors"], json!(["Fanny"]));
            } else {
                assert_eq!(fan["detectedMonitors"], json!([]));
            }
        };

        let out = mock_command(&dir, "status")
            .arg("--json")
            .env("MAC_CLAMSHELL_MOCK_FAN", mock)
            .output()
            .unwrap();
        assert!(out.status.success());
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        check(&v["fan"]);

        let mut cmd = mock_command(&dir, "serve");
        cmd.env("MAC_CLAMSHELL_MOCK_FAN", mock)
            .env("MAC_CLAMSHELL_LANG", "zh-CN");
        let responses = mcp_batch(
            cmd,
            &[
                (
                    "initialize",
                    json!({"protocolVersion": "2025-11-25", "capabilities": app_capabilities()}),
                ),
                (
                    "tools/call",
                    json!({"name": "clamshell_refresh", "arguments": {"force": true}}),
                ),
            ],
        );
        let result = &responses[1]["result"];
        check(&result["structuredContent"]["fan"]);
        let summary = result["content"][0]["text"].as_str().unwrap();
        assert!(summary.contains("风扇"));
        assert!(summary.contains("只读"));
    }

    let text = mock_command(&dir, "status")
        .env("MAC_CLAMSHELL_MOCK_FAN", "active:2140,2210")
        .env("MAC_CLAMSHELL_MOCK_THERM", "unknown")
        .output()
        .unwrap();
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Left 2140 RPM, Right 2210 RPM"));
    assert!(text.contains("unknown"));
    let json_out = mock_command(&dir, "status")
        .arg("--json")
        .env("MAC_CLAMSHELL_MOCK_THERM", "unknown")
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(v["thermalState"], "unknown");
    assert_eq!(v["thermalPressure"], "unknown");
    assert_eq!(v["degraded"], true);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn cli_brightness_open_override_cannot_bypass_real_lid_probe_and_docked_display_is_accurate() {
    let dir = temp_data_dir("p1-lid-display");
    for lid in ["unknown", "closed"] {
        let out = mock_command(&dir, "brightness")
            .args(["set", "85", "--lid-state", "open", "--json"])
            .env("MAC_CLAMSHELL_MOCK_LID", lid)
            .env("MAC_CLAMSHELL_MOCK_DISPLAY", "builtin")
            .env("MAC_CLAMSHELL_MOCK_BRIGHTNESS", "60")
            .output()
            .unwrap();
        assert!(out.status.success());
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["applied"], false, "lid={lid} must never write hardware");
        assert_eq!(v["deferred"], true);
        assert_eq!(v["mode"], "saved_target");
    }

    let docked = mock_command(&dir, "status")
        .arg("--json")
        .env("MAC_CLAMSHELL_MOCK_LID", "closed")
        .env("MAC_CLAMSHELL_MOCK_DISPLAY", "docked")
        .output()
        .unwrap();
    assert!(docked.status.success());
    let dv: Value = serde_json::from_slice(&docked.stdout).unwrap();
    assert_eq!(dv["display"]["lidClosed"], true);
    assert_eq!(dv["display"]["internalOffline"], true);
    assert_eq!(dv["display"]["confirmedAsleep"], false);
    assert_eq!(dv["display"]["asleep"], false);

    // P2 degraded check: unknown lid/power must set degraded: true
    let unk = mock_command(&dir, "status")
        .arg("--json")
        .env("MAC_CLAMSHELL_MOCK_LID", "unknown")
        .env("MAC_CLAMSHELL_MOCK_POWER", "unknown")
        .output()
        .unwrap();
    assert!(unk.status.success());
    let uv: Value = serde_json::from_slice(&unk.stdout).unwrap();
    assert_eq!(uv["degraded"], true);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn mcp_enforces_session_tools_strict_apps_batching_and_lock_verification_status() {
    let dir = temp_data_dir("p1-mcp-proto");

    // 1. Plain session & empty ui extension `{}` reject app-only tools in tools/call
    for caps in [json!({}), json!({"extensions": {UI_EXTENSION: {}}})] {
        let responses = mcp_batch(
            mock_command(&dir, "serve"),
            &[
                (
                    "initialize",
                    json!({"protocolVersion": "2025-03-26", "capabilities": caps}),
                ),
                ("tools/list", json!({})),
                (
                    "tools/call",
                    json!({"name": "clamshell_configure", "arguments": {"guardEnabled": false}}),
                ),
                (
                    "tools/call",
                    json!({"name": "clamshell_refresh", "arguments": {}}),
                ),
            ],
        );
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");
        assert!(responses[0]["result"]["capabilities"]
            .get("extensions")
            .is_none());
        assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 3);
        assert_eq!(responses[2]["error"]["code"], -32602);
        assert_eq!(responses[3]["error"]["code"], -32602);
        assert!(
            !dir.join("settings.json").exists(),
            "rejected configure must not write settings"
        );
    }

    // 2. JSON-RPC 2.0 batching for 2025-03-26 over live stdio transport
    let mut child = mock_command(&dir, "serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}})
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!([
            {"jsonrpc": "2.0", "id": 2, "method": "ping"},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 3, "method": "ping"}
        ])
    )
    .unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<Value> = BufReader::new(out.stdout.as_slice())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    let batch = lines[1].as_array().unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0]["id"], 2);
    assert_eq!(batch[1]["id"], 3);

    // 3. P1 #11 lock screen verified vs submitted reporting in CLI and MCP
    for (mode, expected_verified, expected_status) in [
        ("sac_locked", true, "verified"),
        ("sac_submitted", false, "submitted"),
    ] {
        let cli = mock_command(&dir, "lock")
            .arg("--json")
            .env("MAC_CLAMSHELL_MOCK_DISPLAY", mode)
            .output()
            .unwrap();
        assert!(cli.status.success());
        let cv: Value = serde_json::from_slice(&cli.stdout).unwrap();
        assert_eq!(cv["ok"], true);
        assert_eq!(cv["method"], "SACLockScreenImmediate");
        assert_eq!(cv["verified"], expected_verified);
        assert_eq!(cv["status"], expected_status);

        let mut cmd = mock_command(&dir, "serve");
        cmd.env("MAC_CLAMSHELL_MOCK_DISPLAY", mode);
        let resps = mcp_batch(
            cmd,
            &[
                ("initialize", json!({"protocolVersion": "2025-11-25"})),
                (
                    "tools/call",
                    json!({"name": "clamshell_lock_screen", "arguments": {}}),
                ),
            ],
        );
        let sc = &resps[1]["result"]["structuredContent"];
        assert_eq!(sc["ok"], true);
        assert_eq!(sc["verified"], expected_verified);
        assert_eq!(sc["status"], expected_status);
    }

    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn surviving_mcp_server_takes_over_daemon_lock_and_unloaded_plist_reports_idle() {
    let dir = temp_data_dir("p1-takeover");

    // 1. Unloaded plist on disk alone must NOT cause guardStatus=armed/protecting
    let agents = dir.join("agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("dev.mac-clamshell-mcp.display-guard.plist"),
        "<plist/>",
    )
    .unwrap();
    let unloaded = Command::new(bin_path())
        .args(["status", "--json", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_LAUNCHAGENTS_DIR", &agents)
        .env("MAC_CLAMSHELL_MOCK_LAUNCHCTL", "unloaded")
        .env("MAC_CLAMSHELL_MOCK_LID", "open")
        .env("MAC_CLAMSHELL_MOCK_POWER", "ac")
        .env("MAC_CLAMSHELL_MOCK_BATT", "88")
        .env("MAC_CLAMSHELL_MOCK_THERM", "ok")
        .env("MAC_CLAMSHELL_MOCK_DISPLAY", "builtin")
        .env("MAC_CLAMSHELL_MOCK_FAN", "fanless")
        .output()
        .unwrap();
    assert!(unloaded.status.success());
    let uv: Value = serde_json::from_slice(&unloaded.stdout).unwrap();
    assert_eq!(
        uv["guardStatus"], "idle",
        "unloaded plist without live launchd job or DaemonLock must report idle"
    );
    fs::remove_dir_all(&agents).unwrap();

    // 2. Two live serve processes sharing data_dir: when holder A exits, survivor B takes over
    let mut proc_a = mock_command(&dir, "serve")
        .env("MAC_CLAMSHELL_GUARD_INTERVAL_MS", "100")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin_a = proc_a.stdin.take().unwrap();
    let mut out_a = BufReader::new(proc_a.stdout.take().unwrap());
    writeln!(
        stdin_a,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}})
    )
    .unwrap();
    stdin_a.flush().unwrap();
    let mut line = String::new();
    out_a.read_line(&mut line).unwrap();

    let mut proc_b = mock_command(&dir, "serve")
        .env("MAC_CLAMSHELL_GUARD_INTERVAL_MS", "100")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin_b = proc_b.stdin.take().unwrap();
    let mut out_b = BufReader::new(proc_b.stdout.take().unwrap());
    writeln!(
        stdin_b,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}})
    )
    .unwrap();
    stdin_b.flush().unwrap();
    line.clear();
    out_b.read_line(&mut line).unwrap();

    // Exit A so DaemonLock becomes free; B should take over within a few hundred ms
    drop(stdin_a);
    let _ = proc_a.wait();

    let lock_path = dir.join("display-guard-daemon.lock");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut taken_over = false;
    while Instant::now() < deadline {
        if let Ok(f) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
        {
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                taken_over = true;
                break;
            }
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        taken_over,
        "surviving MCP server B must re-acquire DaemonLock after holder A exits"
    );

    writeln!(
        stdin_b,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "clamshell_panel", "arguments": {"fresh": true}}})
    )
    .unwrap();
    stdin_b.flush().unwrap();
    line.clear();
    out_b.read_line(&mut line).unwrap();
    let panel_b: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        panel_b["result"]["structuredContent"]["guardStatus"],
        "armed"
    );

    drop(stdin_b);
    let _ = proc_b.wait();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sanitization_guard_catches_synthetic_tokens_without_echoing_and_fails_closed() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let patterns_src = repo_root.join(".github/sanitize-patterns.txt");
    let script_src = repo_root.join("scripts/check_sanitized.sh");
    let dir = temp_data_dir("sanitize-guard");

    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(dir.join(".github")).unwrap();
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::copy(&patterns_src, dir.join(".github/sanitize-patterns.txt")).unwrap();
    fs::copy(&script_src, dir.join("scripts/check_sanitized.sh")).unwrap();
    assert!(Command::new("git")
        .args([
            "add",
            ".github/sanitize-patterns.txt",
            "scripts/check_sanitized.sh"
        ])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());

    // 1. Clean repo passes
    let clean = Command::new("bash")
        .arg("scripts/check_sanitized.sh")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        clean.status.success(),
        "expected clean repo to pass: {}",
        String::from_utf8_lossy(&clean.stderr)
    );

    // 2. Dynamically constructed synthetic tokens (never stored as literals in source)
    let akia_tok = format!("{}{}0123456789ABCDEF", "AK", "IA");
    let ghp_tok = format!("{}{}{}", "gh", "p_", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    let sk_tok = format!("{}{}{}", "sk", "-proj-", "AbCdEfGhIjKlMnOpQrStUvWxYz012345");

    for (name, token) in [
        ("akia.txt", akia_tok),
        ("ghp.txt", ghp_tok),
        ("sk.txt", sk_tok),
    ] {
        let sample = dir.join(name);
        fs::write(&sample, format!("export TOKEN={token}\n")).unwrap();
        assert!(Command::new("git")
            .args(["add", name])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());

        // Tracked + staged detection, printing ONLY file:line and zero secret text
        let res = Command::new("bash")
            .arg("scripts/check_sanitized.sh")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(
            res.status.code(),
            Some(1),
            "token {name} must fail with exit 1"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&res.stdout),
            String::from_utf8_lossy(&res.stderr)
        );
        assert!(
            combined.contains(&format!("{name}:1")),
            "expected {name}:1 in output, got: {combined}"
        );
        assert!(
            !combined.contains(&token),
            "secret token text must NEVER be echoed in output: {combined}"
        );

        // Overwrite working tree to clean while secret stays staged -> still caught via --cached
        fs::write(&sample, "clean = true\n").unwrap();
        let staged_res = Command::new("bash")
            .arg("scripts/check_sanitized.sh")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(
            staged_res.status.code(),
            Some(1),
            "staged-only secret {name} must still fail with exit 1"
        );
        let staged_combined = format!(
            "{}{}",
            String::from_utf8_lossy(&staged_res.stdout),
            String::from_utf8_lossy(&staged_res.stderr)
        );
        assert!(staged_combined.contains(&format!("{name}:1")));
        assert!(!staged_combined.contains(&token));

        assert!(Command::new("git")
            .args(["rm", "-fq", name])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());
    }

    // 3. Fail-closed (exit 2) on empty patterns or invalid regex
    fs::write(
        dir.join(".github/sanitize-patterns.txt"),
        "# only comments\n\n",
    )
    .unwrap();
    let empty_res = Command::new("bash")
        .arg("scripts/check_sanitized.sh")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(empty_res.status.code(), Some(2));

    fs::write(
        dir.join(".github/sanitize-patterns.txt"),
        "[unclosed_regex\n",
    )
    .unwrap();
    let bad_re_res = Command::new("bash")
        .arg("scripts/check_sanitized.sh")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(bad_re_res.status.code(), Some(2));

    fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "test-mocks")]
#[test]
fn mcp_stdio_caps_oversize_lines_and_freezes_session_on_duplicate_initialize() {
    let dir = temp_data_dir("mcp-hardening");
    let mut child = mock_command(&dir, "serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut send = |raw: &str| -> Value {
        writeln!(stdin, "{raw}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    // 1. Initialize on 2025-06-18 (non-apps)
    let init1 = send(
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}).to_string(),
    );
    assert_eq!(init1["result"]["protocolVersion"], "2025-06-18");

    // 2. Duplicate initialize rejected with -32600
    let init2 = send(
        &json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":app_capabilities()}}).to_string(),
    );
    assert_eq!(init2["error"]["code"], -32600);

    // 3. Batch array rejected on 2025-06-18
    let batch = send(&json!([{"jsonrpc":"2.0","id":3,"method":"ping"}]).to_string());
    assert_eq!(batch["error"]["code"], -32600);

    // 4. Oversize >1 MiB line rejected with -32600 and server stays healthy for next ping
    let huge = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"ping","params":{{"pad":"{}"}}}}"#,
        "a".repeat(1024 * 1024 + 64)
    );
    let too_big = send(&huge);
    assert_eq!(too_big["error"]["code"], -32600);

    let ping = send(&json!({"jsonrpc":"2.0","id":5,"method":"ping"}).to_string());
    assert_eq!(ping["id"], 5);
    assert_eq!(ping["result"], json!({}));

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(not(feature = "test-mocks"))]
#[test]
fn production_binary_ignores_mock_env_vars() {
    let dir = temp_data_dir("prod-no-mocks");
    let out = Command::new(bin_path())
        .args(["status", "--json", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_MOCK_FAN", "yield:BogusMockController:9999")
        .env("MAC_CLAMSHELL_MOCK_TOPOLOGY", "intel:99")
        .env("MAC_CLAMSHELL_MOCK_HOST", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(v["fan"]["controller"], "BogusMockController");
    assert_ne!(v["cpuTopology"]["total"], 99);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn install_scripts_enforce_security_hardening() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let install_sh = fs::read_to_string(repo_root.join("scripts/install.sh")).unwrap();
    let install_plugin_sh =
        fs::read_to_string(repo_root.join("scripts/install-plugin.sh")).unwrap();
    let install_agent_sh =
        fs::read_to_string(repo_root.join("scripts/install-launchagent.sh")).unwrap();

    // Structural checks on install.sh & install-plugin.sh
    for (name, body) in [
        ("scripts/install.sh", &install_sh),
        ("scripts/install-plugin.sh", &install_plugin_sh),
    ] {
        assert!(
            body.contains("^(localhost|127\\.0\\.0\\.1|\\[::1\\]):[0-9]{1,5}$"),
            "{name} must validate ANTIGRAVITY_LS_ADDRESS against loopback regex"
        );
        assert!(
            body.contains("curl -sf --connect-timeout 2 --max-time 5 -K -"),
            "{name} must pass CSRF header via curl -K - with timeouts"
        );
        assert!(
            !body.contains("-H \"x-codeium-csrf-token"),
            "{name} must never pass x-codeium-csrf-token via -H argv"
        );
    }
    assert!(install_sh.contains("^v[0-9]+\\.[0-9]+\\.[0-9]+([-.][0-9A-Za-z.]+)?$"));
    assert!(install_sh.contains("^[0-9a-f]{64}$"));
    assert!(install_sh.contains("find \"$STAGE\" -type l"));

    // Structural checks on install-launchagent.sh
    assert!(install_agent_sh.contains("[ -L \"$PLIST_PATH\" ]"));
    assert!(install_agent_sh.contains("[:cntrl:]"));
    assert!(install_agent_sh.contains("umask 077"));
    assert!(install_agent_sh.contains("mktemp"));
    assert!(install_agent_sh.contains("chmod 0600"));
    assert!(install_agent_sh.contains("/usr/bin/plutil -lint \"$TMP_PLIST\""));
    assert!(install_agent_sh.contains("mv -f \"$TMP_PLIST\" \"$PLIST_PATH\""));

    // Behavioral checks: install.sh rejects traversal versions before network
    let bad_ver = Command::new("sh")
        .arg(repo_root.join("scripts/install.sh"))
        .args(["--version", "v1.0.0/../../evil"])
        .output()
        .unwrap();
    assert_eq!(bad_ver.status.code(), Some(1));

    // Behavioral checks: install-launchagent.sh rejects XML quotes & symlinked plist target
    let dir = temp_data_dir("install-agent-sec");
    let fake_home = dir.join("home");
    let agents_dir = fake_home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents_dir).unwrap();
    let target_file = dir.join("victim.txt");
    fs::write(&target_file, "untouched").unwrap();
    let plist_link = agents_dir.join("dev.mac-clamshell-mcp.display-guard.plist");
    std::os::unix::fs::symlink(&target_file, &plist_link).unwrap();

    let sym_res = Command::new("sh")
        .arg(repo_root.join("scripts/install-launchagent.sh"))
        .env("HOME", &fake_home)
        .args(["--bin", "/bin/echo", "--data-dir"])
        .arg(dir.join("data"))
        .output()
        .unwrap();
    assert_eq!(sym_res.status.code(), Some(1));
    assert_eq!(fs::read_to_string(&target_file).unwrap(), "untouched");

    let quote_res = Command::new("sh")
        .arg(repo_root.join("scripts/install-launchagent.sh"))
        .env("HOME", &fake_home)
        .args(["--bin", "/bin/echo", "--data-dir", "/tmp/bad'dir"])
        .output()
        .unwrap();
    assert_eq!(quote_res.status.code(), Some(2));

    let nl_res = Command::new("sh")
        .arg(repo_root.join("scripts/install-launchagent.sh"))
        .env("HOME", &fake_home)
        .args(["--bin", "/bin/echo", "--data-dir", "/tmp/bad\ndir"])
        .output()
        .unwrap();
    assert_eq!(nl_res.status.code(), Some(2));

    let _ = fs::remove_dir_all(&dir);
}
