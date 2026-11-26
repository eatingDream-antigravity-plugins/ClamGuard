use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

const UI_EXTENSION: &str = "io.modelcontextprotocol/ui";
const UI_MIME: &str = "text/html;profile=mcp-app";

fn app_capabilities() -> Value {
    json!({"extensions": {UI_EXTENSION: {"mimeTypes": [UI_MIME]}}})
}

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
        .env("MAC_CLAMSHELL_LANG", "en");
    command
}

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
        .contains("read-only, yielding to Macs Fan Control"));
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
            assert!(res.get("structuredContent").is_none());
            assert!(res["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(message));
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

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

#[test]
fn fan_samples_are_read_only_in_cli_and_mcp_with_bilingual_text() {
    let dir = temp_data_dir("fans");
    for (mock, status, controller, ownership, expected_rpms) in [
        ("fanless", "fanless", "System (macOS)", "observer", vec![]),
        (
            "active:2140,2210",
            "ok",
            "System (macOS)",
            "observer",
            vec![2140, 2210],
        ),
        (
            "yield:Macs Fan Control:2400,2450",
            "ok",
            "Macs Fan Control",
            "yield",
            vec![2400, 2450],
        ),
        ("manual:2100", "ok", "Manual (SMC)", "yield", vec![2100]),
        (
            "unavailable",
            "unavailable",
            "System (macOS)",
            "observer",
            vec![],
        ),
    ] {
        let check = |fan: &Value| {
            assert_eq!(fan["status"], status);
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
