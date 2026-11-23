use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_SEQ: AtomicU64 = AtomicU64::new(1);

fn temp_data_dir(tag: &str) -> PathBuf {
    let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "mac-clamshell-it-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_mac-clamshell-mcp")
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
    let mut child = Command::new(bin_path())
        .args(["serve", "--data-dir"])
        .arg(&dir)
        .env("MAC_CLAMSHELL_MOCK_LID", "closed")
        .env("MAC_CLAMSHELL_MOCK_POWER", "battery")
        .env("MAC_CLAMSHELL_MOCK_BATT", "88")
        .env("MAC_CLAMSHELL_MOCK_THERM", "ok")
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

    let init = rpc(1, "initialize", json!({"protocolVersion": "2024-11-05"}));
    assert_eq!(init["result"]["serverInfo"]["name"], "mac-clamshell-mcp");

    let tools = rpc(2, "tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 4);

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

    drop(stdin);
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}
