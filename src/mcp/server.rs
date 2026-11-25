use crate::mcp::telemetry::TelemetryEngine;
use crate::mcp::tools::{
    call_tool_json, list_resources_json, list_tools_json, read_resource_json, HtmlLoader,
    SERVER_NAME, SERVER_VERSION,
};
use crate::platform::display_ffi::{DisplayBackend, MacOsDisplayBackend};
use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DEFAULT_SERVE_GUARD_INTERVAL_MS: u64 = 5_000;

pub fn handle_rpc_value<P: ProbeBackend + 'static, D: DisplayBackend + 'static>(
    msg: &Value,
    engine: &Arc<TelemetryEngine<P, D>>,
    html_loader: &HtmlLoader,
) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

    // Notifications (no id) require no JSON-RPC response.
    let id = id?;

    let ok = |result: Value| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
    };
    let err = |code: i64, message: String| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        })
    };

    match method {
        "initialize" => {
            let client_proto = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("2024-11-05");
            Some(ok(json!({
                "protocolVersion": client_proto,
                "capabilities": {
                    "tools": {},
                    "resources": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            })))
        }
        "ping" => Some(ok(json!({}))),
        "resources/list" => Some(ok(list_resources_json())),
        "resources/read" => {
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            match read_resource_json(uri, engine, html_loader) {
                Ok(res) => Some(ok(res)),
                Err(e) => Some(err(-32602, e)),
            }
        }
        "tools/list" => Some(ok(list_tools_json())),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Some(ok(call_tool_json(name, &args, engine)))
        }
        other => Some(err(-32601, format!("Method not found: {other}"))),
    }
}

pub fn serve_stdio(data_dir: PathBuf, panel_html_override: Option<PathBuf>) -> io::Result<()> {
    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "mac-clamshell-mcp".to_string());

    let engine = Arc::new(TelemetryEngine::new(
        data_dir,
        binary_path,
        MacOsProbeBackend,
        MacOsDisplayBackend,
    ));
    engine.set_bg_guard_active(true);
    let html_loader = Arc::new(HtmlLoader::new(panel_html_override));

    // Spawn lightweight lock-coordinated Display Guard loop so closed-lid backlight protection,
    // wake-restore, and optional thermal brightness cap work out-of-the-box whenever Antigravity is open.
    let stop_guard = Arc::new(AtomicBool::new(false));
    let guard_interval_ms = std::env::var("MAC_CLAMSHELL_GUARD_INTERVAL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SERVE_GUARD_INTERVAL_MS)
        .clamp(100, 300_000);
    let guard_handle = {
        let e = Arc::clone(&engine);
        let stop = Arc::clone(&stop_guard);
        std::thread::Builder::new()
            .name("clamshell-guard".into())
            .spawn(move || {
                let normal_dur = Duration::from_millis(guard_interval_ms);
                let fast_dur = Duration::from_millis(
                    guard_interval_ms.min(crate::guard::FAST_POLL_INTERVAL_MS),
                );
                while !stop.load(Ordering::SeqCst) {
                    let fast = e.run_background_guard_tick();
                    e.wait_guard_interval(if fast { fast_dur } else { normal_dur }, &stop);
                }
            })
            .ok()
    };

    // Pre-warm HTML template and initial telemetry asynchronously so first open is <0.5ms.
    {
        let e = Arc::clone(&engine);
        let h = Arc::clone(&html_loader);
        let _ = std::thread::Builder::new()
            .name("clamshell-prewarm".into())
            .spawn(move || {
                h.prewarm();
                e.prewarm();
            });
    }

    let stdin = io::stdin();
    let stdout = Arc::new(Mutex::new(BufWriter::new(io::stdout())));
    let mut line = String::new();
    let mut reader = stdin.lock();
    let mut io_res = Ok(());

    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(e) => {
                io_res = Err(e);
                break;
            }
        };
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(parse_err) => {
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {parse_err}") }
                });
                let mut out = stdout.lock().unwrap_or_else(|e| e.into_inner());
                let _ = serde_json::to_writer(&mut *out, &err_resp);
                let _ = out.write_all(b"\n");
                let _ = out.flush();
                continue;
            }
        };

        if let Some(resp) = handle_rpc_value(&msg, &engine, &html_loader) {
            let mut out = stdout.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = serde_json::to_writer(&mut *out, &resp)
                .map_err(io::Error::other)
                .and_then(|_| out.write_all(b"\n"))
                .and_then(|_| out.flush())
            {
                io_res = Err(e);
                break;
            }
        }
    }

    stop_guard.store(true, Ordering::SeqCst);
    engine.notify_guard_wake();
    if let Some(h) = guard_handle {
        let _ = h.join();
    }

    io_res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::macos::{HardwareProbe, MemorySummary, StaticHostInfo};
    use crate::policy::CpuTopology;
    use std::sync::Mutex;

    struct TestProbe {
        lid_closed: bool,
    }
    impl ProbeBackend for TestProbe {
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
            Some(self.lid_closed)
        }
        fn probe_hardware(&self) -> HardwareProbe {
            HardwareProbe {
                lid_state: if self.lid_closed { "closed" } else { "open" }.into(),
                clamshell_causes_sleep: "no".into(),
                power_source: "battery".into(),
                battery_pct: Some(88),
                battery_detail: "-InternalBattery-0\t88%; discharging".into(),
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
                load_avg: [1.2, 1.1, 1.0],
                uptime_seconds: 7200,
                degraded: false,
            }
        }
    }

    struct TestDisplay {
        builtins: Vec<u32>,
        level: Mutex<f32>,
    }
    impl DisplayBackend for TestDisplay {
        fn get_displays(&self) -> (Vec<u32>, Vec<u32>) {
            (self.builtins.clone(), vec![])
        }
        fn is_asleep(&self, _id: u32) -> Option<bool> {
            Some(false)
        }
        fn get_brightness(&self, _id: u32) -> Option<f32> {
            if self.builtins.is_empty() {
                None
            } else {
                Some(*self.level.lock().unwrap())
            }
        }
        fn set_brightness(&self, _id: u32, level: f32) -> bool {
            if self.builtins.is_empty() {
                return false;
            }
            *self.level.lock().unwrap() = level;
            true
        }
        fn sleep_display_now(&self) -> bool {
            true
        }
        fn lock_screen_now(&self) -> Result<&'static str, String> {
            Ok("mock")
        }
    }

    #[test]
    fn mcp_rpc_end_to_end_ssr_dedup_configure_brightness_and_desktop_error() {
        let dir =
            std::env::temp_dir().join(format!("mac-clamshell-mcp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let engine = Arc::new(TelemetryEngine::new(
            dir.clone(),
            "mac-clamshell-mcp".into(),
            TestProbe { lid_closed: true },
            TestDisplay {
                builtins: vec![1],
                level: Mutex::new(0.0),
            },
        ));
        engine.set_bg_guard_active(true);
        let html_loader = HtmlLoader::new(None);

        let init = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "mac-clamshell-mcp");

        let tools = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "clamshell_panel",
                "clamshell_refresh",
                "clamshell_configure",
                "clamshell_set_brightness",
                "clamshell_lock_screen"
            ]
        );
        // Verify guardEnabled is exposed in English tools/list schema
        assert_eq!(
            tools["result"]["tools"][2]["inputSchema"]["properties"]["guardEnabled"]["type"],
            "boolean"
        );

        let res = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"ui://mac-clamshell-mcp/panel"}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        let html = res["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("window.__MAC_CLAMSHELL_BOOT__="));
        assert!(html.contains("\"display\":"));
        assert!(!html.contains("/*__MAC_CLAMSHELL_BOOT__*/"));

        let panel = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"clamshell_panel","arguments":{}}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        let hash = panel["result"]["structuredContent"]["hash"]
            .as_str()
            .unwrap()
            .to_string();

        // Matching ifHash returns minimal unchanged: true (including fresh: true live bypass)
        let poll = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"clamshell_refresh","arguments":{"ifHash":hash,"force":false}}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        assert_eq!(poll["result"]["structuredContent"]["unchanged"], true);
        assert_eq!(poll["result"]["content"][0]["text"], "unchanged");

        let fresh_poll = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":51,"method":"tools/call","params":{"name":"clamshell_refresh","arguments":{"ifHash":hash,"fresh":true}}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        assert_eq!(fresh_poll["result"]["structuredContent"]["unchanged"], true);
        assert_eq!(fresh_poll["result"]["structuredContent"]["fromCache"], false);

        let fresh_panel = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":52,"method":"tools/call","params":{"name":"clamshell_panel","arguments":{"fresh":true}}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        assert_eq!(fresh_panel["result"]["structuredContent"]["fromCache"], false);

        // Lock screen tool returns { ok: true, method, locked_at_ms }
        let locked = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":53,"method":"tools/call","params":{"name":"clamshell_lock_screen","arguments":{}}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        assert!(locked["result"].get("isError").is_none());
        assert_eq!(locked["result"]["structuredContent"]["ok"], true);
        assert_eq!(locked["result"]["structuredContent"]["method"], "mock");
        assert!(
            locked["result"]["structuredContent"]["locked_at_ms"]
                .as_u64()
                .unwrap_or(0)
                > 0
        );

        // Closed-lid set_brightness updates saved restore target safely without waking backlight
        let br = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"clamshell_set_brightness","arguments":{"percent":45}}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        assert!(br["result"].get("isError").is_none());
        assert_eq!(
            br["result"]["structuredContent"]["brightnessAction"]["mode"],
            "saved_target"
        );
        assert_eq!(
            br["result"]["structuredContent"]["brightnessAction"]["savedPercent"],
            45
        );

        // Configure policy + guardEnabled toggle updates profile, guardStatus, and hash
        let cfg = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"clamshell_configure","arguments":{"modeOverride":"utility","maxJobsOverride":2,"guardEnabled":false}}}),
            &engine,
            &html_loader,
        )
        .unwrap();
        assert_eq!(
            cfg["result"]["structuredContent"]["policy"]["profile"],
            "utility"
        );
        assert_eq!(
            cfg["result"]["structuredContent"]["guardStatus"],
            "paused"
        );
        assert_ne!(
            cfg["result"]["structuredContent"]["hash"].as_str().unwrap(),
            hash
        );

        // Desktop Mac (open/unknown lid + no built-in display) returns explicit isError: true
        let desktop_engine = Arc::new(TelemetryEngine::new(
            dir.join("desktop"),
            "mac-clamshell-mcp".into(),
            TestProbe { lid_closed: false },
            TestDisplay {
                builtins: vec![],
                level: Mutex::new(0.0),
            },
        ));
        let desktop_err = handle_rpc_value(
            &json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"clamshell_set_brightness","arguments":{"percent":50}}}),
            &desktop_engine,
            &html_loader,
        )
        .unwrap();
        assert_eq!(desktop_err["result"]["isError"], true);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
