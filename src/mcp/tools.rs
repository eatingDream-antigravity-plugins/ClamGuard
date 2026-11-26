use crate::i18n::{parse_lang_tag, Lang};
use crate::mcp::telemetry::{summarize_for_model, TelemetryEngine};
use crate::platform::display_ffi::{clamp_percent, DisplayBackend};
use crate::platform::macos::ProbeBackend;
use crate::state::SettingsPatch;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, UNIX_EPOCH};

pub const APP_URI: &str = "ui://mac-clamshell-mcp/panel";
pub const APP_MIME: &str = "text/html;profile=mcp-app";
pub const SERVER_NAME: &str = "mac-clamshell-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const BOOT_PLACEHOLDER: &str = "/*__MAC_CLAMSHELL_BOOT__*/";

const EMBEDDED_PANEL_HTML: &str = include_str!("../../panel/dist/panel.html");

pub struct CompiledTemplate {
    prefix: String,
    suffix: String,
    has_placeholder: bool,
}

impl CompiledTemplate {
    fn from_html(html: &str) -> Self {
        if let Some((prefix, suffix)) = html.split_once(BOOT_PLACEHOLDER) {
            Self {
                prefix: prefix.to_string(),
                suffix: suffix.to_string(),
                has_placeholder: true,
            }
        } else {
            Self {
                prefix: html.to_string(),
                suffix: String::new(),
                has_placeholder: false,
            }
        }
    }

    pub fn render_with_boot(&self, boot_script: &str) -> String {
        if !self.has_placeholder {
            return self.prefix.clone();
        }
        let mut out =
            String::with_capacity(self.prefix.len() + boot_script.len() + self.suffix.len());
        out.push_str(&self.prefix);
        out.push_str(boot_script);
        out.push_str(&self.suffix);
        out
    }
}

struct CachedHtml {
    path: PathBuf,
    mtime_ns: u128,
    size: u64,
    template: Arc<CompiledTemplate>,
}

pub struct HtmlLoader {
    candidate_paths: Vec<PathBuf>,
    embedded: Arc<CompiledTemplate>,
    cache: Mutex<Option<CachedHtml>>,
}

impl HtmlLoader {
    pub fn new(override_path: Option<PathBuf>) -> Self {
        let mut candidate_paths = Vec::with_capacity(3);
        if let Some(p) = override_path {
            candidate_paths.push(p);
        }
        if let Ok(env_p) = std::env::var("MAC_CLAMSHELL_PANEL_HTML") {
            let trimmed = env_p.trim();
            if !trimmed.is_empty() {
                candidate_paths.push(PathBuf::from(trimmed));
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(plugin_root) = exe.parent().and_then(Path::parent) {
                candidate_paths.push(plugin_root.join("panel").join("dist").join("panel.html"));
            }
        }
        Self {
            candidate_paths,
            embedded: Arc::new(CompiledTemplate::from_html(EMBEDDED_PANEL_HTML)),
            cache: Mutex::new(None),
        }
    }

    pub fn prewarm(&self) {
        let _ = self.load();
    }

    pub fn load(&self) -> Arc<CompiledTemplate> {
        let mut found: Option<(&PathBuf, u128, u64)> = None;
        for p in &self.candidate_paths {
            if let Ok(meta) = fs::metadata(p) {
                if meta.is_file() {
                    let size = meta.len();
                    let mtime_ns = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos())
                        .unwrap_or(0);
                    found = Some((p, mtime_ns, size));
                    break;
                }
            }
        }

        let Some((path, mtime_ns, size)) = found else {
            return Arc::clone(&self.embedded);
        };

        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref c) = *cache {
            if c.path == *path && c.mtime_ns == mtime_ns && c.size == size {
                return Arc::clone(&c.template);
            }
        }
        match fs::read_to_string(path) {
            Ok(text) if !text.is_empty() => {
                let template = Arc::new(CompiledTemplate::from_html(&text));
                *cache = Some(CachedHtml {
                    path: path.clone(),
                    mtime_ns,
                    size,
                    template: Arc::clone(&template),
                });
                template
            }
            _ => Arc::clone(&self.embedded),
        }
    }
}

pub fn serialize_boot_data(val: &Value) -> String {
    let raw = serde_json::to_string(val).unwrap_or_else(|_| "null".to_string());
    let mut out = String::with_capacity(raw.len() + 32);
    for ch in raw.chars() {
        match ch {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

pub fn list_resources_json() -> Value {
    json!({
        "resources": [
            {
                "uri": APP_URI,
                "name": "ClamGuard Dashboard",
                "description": "Live macOS lid state, power, thermal pressure, performance throttling, display guard, and built-in brightness controls.",
                "mimeType": APP_MIME,
                "_meta": {
                    "ui": { "prefersBorder": false }
                }
            }
        ]
    })
}

pub fn read_resource_json<P: ProbeBackend + 'static, D: DisplayBackend + 'static>(
    uri: &str,
    engine: &Arc<TelemetryEngine<P, D>>,
    html_loader: &HtmlLoader,
) -> Result<Value, String> {
    if uri != APP_URI {
        return Err(format!("Unknown resource URI: {uri}"));
    }
    let template = html_loader.load();
    let collected = engine.collect(false);
    let mut boot_val = serde_json::to_value(&*collected.snapshot).unwrap_or_else(|_| json!({}));
    if let Some(obj) = boot_val.as_object_mut() {
        obj.insert("unchanged".into(), json!(false));
        obj.insert("serverMs".into(), json!(collected.server_ms));
        obj.insert("fromCache".into(), json!(collected.from_cache));
        obj.insert("stale".into(), json!(collected.stale));
        obj.insert("serverVersion".into(), json!(SERVER_VERSION));
    }
    let boot_script = format!(
        "window.__MAC_CLAMSHELL_BOOT__={};",
        serialize_boot_data(&boot_val)
    );
    let html = template.render_with_boot(&boot_script);

    Ok(json!({
        "contents": [
            {
                "uri": APP_URI,
                "mimeType": APP_MIME,
                "text": html,
                "_meta": {
                    "ui": { "prefersBorder": false }
                }
            }
        ]
    }))
}

pub fn is_tool_allowed(name: &str, apps_capable: bool) -> bool {
    match name {
        "clamshell_panel" | "clamshell_set_brightness" | "clamshell_lock_screen" => true,
        "clamshell_refresh" | "clamshell_configure" => apps_capable,
        _ => false,
    }
}

pub fn list_tools_json(apps_capable: bool) -> Value {
    let mut result = json!({
        "tools": [
            {
                "name": "clamshell_panel",
                "title": "ClamGuard",
                "description": "Open the ClamGuard live dashboard showing macOS lid state, power/battery, thermal pressure, read-only fan telemetry, built-in display brightness, display guard status, CPU/memory load, and active throttling profile.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "fresh": {
                            "type": "boolean",
                            "description": "Bypass short-lived telemetry cache and probe macOS immediately."
                        }
                    },
                    "additionalProperties": false
                },
                "_meta": {
                    "ui": { "resourceUri": APP_URI },
                    "com.google.antigravity/ui": {
                        "entrypoints": [
                            { "type": "global", "title": "ClamGuard" },
                            { "type": "aux_pane", "title": "ClamGuard" }
                        ]
                    }
                }
            },
            {
                "name": "clamshell_refresh",
                "title": "Refresh Clamshell Telemetry",
                "description": "Fetch fresh macOS lid, power, thermal pressure, fan readings, display brightness, guard status, and policy telemetry. Pass ifHash to receive a minimal { unchanged: true } payload when state has not changed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ifHash": {
                            "type": "string",
                            "maxLength": 64,
                            "description": "Previous telemetry hash for zero-overhead ETag deduplication."
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Bypass short-lived telemetry cache and probe macOS immediately."
                        },
                        "fresh": {
                            "type": "boolean",
                            "description": "Alias for force: bypass short-lived telemetry cache and probe macOS immediately."
                        }
                    },
                    "additionalProperties": false
                },
                "_meta": {
                    "ui": { "visibility": ["app"] }
                }
            },
            {
                "name": "clamshell_configure",
                "title": "Configure Clamshell Policy & Guard",
                "description": "Persist performance mode override, max_jobs cap, closed-AC default profile, display guard enable/pause toggle, thermal brightness cap, or language preference and return updated telemetry.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "modeOverride": {
                            "type": "string",
                            "enum": ["auto", "eco", "utility", "strict", "normal"],
                            "description": "Performance profile override (auto follows live lid, power, and thermal pressure)."
                        },
                        "maxJobsOverride": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 64,
                            "description": "Explicit parallel job limit override (0 restores automatic profile default)."
                        },
                        "acClosedProfile": {
                            "type": "string",
                            "enum": ["utility", "eco"],
                            "description": "Default profile used when the lid is closed on AC power."
                        },
                        "guardEnabled": {
                            "type": "boolean",
                            "description": "Enable or pause the closed-lid display backlight and thermal guard."
                        },
                        "thermalBrightCapPct": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 100,
                            "description": "Optional open-lid brightness cap (1-100%) applied under serious/critical thermal pressure; 0 disables."
                        },
                        "lang": {
                            "type": "string",
                            "enum": ["auto", "en", "zh-CN"],
                            "description": "Preferred UI and CLI language override (auto, en, or zh-CN)."
                        }
                    },
                    "additionalProperties": false
                },
                "_meta": {
                    "ui": { "visibility": ["app"] }
                }
            },
            {
                "name": "clamshell_set_brightness",
                "title": "Set Built-in Display Brightness",
                "description": "Adjust MacBook built-in display brightness (0-100%) immediately when the lid is open and the display is awake. Otherwise saves the wake-restore target. Returns an error on desktop Macs without a built-in display.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "percent": {
                            "type": "number",
                            "minimum": 0,
                            "maximum": 100,
                            "description": "Target built-in display brightness percentage (0 to 100)."
                        }
                    },
                    "required": ["percent"],
                    "additionalProperties": false
                }
            },
            {
                "name": "clamshell_lock_screen",
                "title": "Lock Mac Screen",
                "description": "Lock the macOS screen immediately without putting the Mac to sleep, keeping background builds and AI agents running. Uses native SACLockScreenImmediate with CGSession fallback. The pmset fallback requires a verified macOS session lock.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        ]
    });
    if let Some(tools) = result["tools"].as_array_mut() {
        tools.retain(|tool| {
            tool["name"]
                .as_str()
                .is_some_and(|name| is_tool_allowed(name, apps_capable))
        });
    }
    result
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RefreshArgs {
    #[serde(default)]
    if_hash: Option<String>,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    fresh: Option<bool>,
}

pub fn call_tool_json<P: ProbeBackend + 'static, D: DisplayBackend + 'static>(
    name: &str,
    args: &Value,
    engine: &Arc<TelemetryEngine<P, D>>,
) -> Value {
    match name {
        "clamshell_panel" => {
            let fresh = args.get("fresh").and_then(|v| v.as_bool()).unwrap_or(false);
            let collected = engine.collect(fresh);
            let mut sc = serde_json::to_value(&*collected.snapshot).unwrap_or_else(|_| json!({}));
            if let Some(obj) = sc.as_object_mut() {
                obj.insert("unchanged".into(), json!(false));
                obj.insert("serverMs".into(), json!(collected.server_ms));
                obj.insert("fromCache".into(), json!(collected.from_cache));
                obj.insert("stale".into(), json!(collected.stale));
            }
            json!({
                "content": [{ "type": "text", "text": summarize_for_model(&collected.snapshot) }],
                "structuredContent": sc
            })
        }
        "clamshell_refresh" => {
            let parsed: RefreshArgs = match serde_json::from_value(args.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": format!("Invalid refresh arguments: {e}") }]
                    });
                }
            };
            if let Some(ref h) = parsed.if_hash {
                if h.chars().count() > 64 {
                    return json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": "Invalid refresh arguments: ifHash must be a string with maxLength 64" }]
                    });
                }
            }
            let force = parsed.force.unwrap_or(false) || parsed.fresh.unwrap_or(false);
            let (unchanged, sc, snap) = engine.refresh_json(parsed.if_hash.as_deref(), force);
            let text = if unchanged {
                "unchanged".to_string()
            } else {
                summarize_for_model(&snap)
            };
            json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": sc
            })
        }
        "clamshell_configure" => {
            let patch: SettingsPatch = match serde_json::from_value(args.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": format!("Invalid configuration arguments: {e}") }]
                    });
                }
            };
            match engine.configure(&patch) {
                Ok(_) => {
                    let collected = engine.collect(true);
                    let mut sc =
                        serde_json::to_value(&*collected.snapshot).unwrap_or_else(|_| json!({}));
                    if let Some(obj) = sc.as_object_mut() {
                        obj.insert("unchanged".into(), json!(false));
                        obj.insert("serverMs".into(), json!(collected.server_ms));
                        obj.insert("fromCache".into(), json!(collected.from_cache));
                        obj.insert("stale".into(), json!(collected.stale));
                    }
                    json!({
                        "content": [{ "type": "text", "text": summarize_for_model(&collected.snapshot) }],
                        "structuredContent": sc
                    })
                }
                Err(err) => {
                    let msg = match engine.detect_lang() {
                        Lang::ZhCn => format!("更新配置失败: {err}"),
                        Lang::En => format!("Configuration update failed: {err}"),
                    };
                    json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": msg }]
                    })
                }
            }
        }
        "clamshell_set_brightness" => {
            if let Some(obj) = args.as_object() {
                if let Some(unknown) = obj.keys().find(|k| k.as_str() != "percent") {
                    return json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": format!("Invalid brightness arguments: unknown field `{unknown}`") }]
                    });
                }
            }
            let Some(raw_pct) = args.get("percent").and_then(|v| v.as_f64()) else {
                let msg = match engine.detect_lang() {
                    Lang::ZhCn => "亮度调节失败: percent 必须是 0 到 100 之间的数字".to_string(),
                    Lang::En => {
                        "Brightness update failed: percent must be a finite number between 0 and 100"
                            .to_string()
                    }
                };
                return json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": msg }]
                });
            };
            let clamped = match clamp_percent(raw_pct) {
                Ok(v) => v,
                Err(err) => {
                    return json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": format!("Brightness update failed: {err}") }]
                    });
                }
            };
            let t0 = Instant::now();
            match engine.set_brightness(clamped as f64, None) {
                Ok(action) => {
                    if action.mode == "unavailable" {
                        return json!({
                            "isError": true,
                            "content": [{ "type": "text", "text": "Brightness update failed: no built-in display detected on this Mac" }]
                        });
                    }
                    let mut collected = engine.collect(true);
                    collected.server_ms = t0.elapsed().as_millis() as u64;
                    let lang = parse_lang_tag(&collected.snapshot.locale).unwrap_or(Lang::En);
                    let mut sc =
                        serde_json::to_value(&*collected.snapshot).unwrap_or_else(|_| json!({}));
                    if let Some(obj) = sc.as_object_mut() {
                        obj.insert("unchanged".into(), json!(false));
                        obj.insert("serverMs".into(), json!(collected.server_ms));
                        obj.insert("fromCache".into(), json!(collected.from_cache));
                        obj.insert("stale".into(), json!(collected.stale));
                        obj.insert(
                            "brightnessAction".into(),
                            serde_json::to_value(&action).unwrap_or_else(|_| json!({})),
                        );
                    }
                    let note = match (lang, action.mode.as_str()) {
                        (Lang::ZhCn, "saved_target") => {
                            format!("已保存开盖恢复亮度目标: {}%", action.saved_percent)
                        }
                        (Lang::ZhCn, _) => {
                            format!("内建屏幕亮度已设为 {}%", action.percent.unwrap_or(clamped))
                        }
                        (Lang::En, "saved_target") => format!(
                            "Saved lid-open restore brightness target: {}%",
                            action.saved_percent
                        ),
                        (Lang::En, _) => format!(
                            "Built-in display brightness set to {}%",
                            action.percent.unwrap_or(clamped)
                        ),
                    };
                    json!({
                        "content": [{ "type": "text", "text": format!("{note}\n{}", summarize_for_model(&collected.snapshot)) }],
                        "structuredContent": sc
                    })
                }
                Err(err) => {
                    let msg = match engine.detect_lang() {
                        Lang::ZhCn => format!("亮度调节失败: {err}"),
                        Lang::En => format!("Brightness update failed: {err}"),
                    };
                    json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": msg }]
                    })
                }
            }
        }
        "clamshell_lock_screen" => match engine.lock_screen() {
            Ok((outcome, locked_at_ms, server_ms)) => {
                let method = outcome.method;
                let verified = outcome.verified;
                let status = if verified { "verified" } else { "submitted" };
                let text = match (engine.detect_lang(), verified) {
                    (Lang::ZhCn, true) => {
                        format!("已锁屏（{method}，已核验会话锁），Mac 将保持唤醒以继续运行后台任务。")
                    }
                    (Lang::ZhCn, false) => {
                        format!("已提交锁屏请求（{method}，未核验会话锁状态），Mac 将保持唤醒。")
                    }
                    (Lang::En, true) => format!(
                        "Screen locked ({method}, verified); Mac remains awake for background tasks."
                    ),
                    (Lang::En, false) => format!(
                        "Lock screen request submitted ({method}, session lock not verified); Mac remains awake."
                    ),
                };
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "structuredContent": {
                        "ok": true,
                        "method": method,
                        "verified": verified,
                        "status": status,
                        "locked_at_ms": locked_at_ms,
                        "serverMs": server_ms
                    }
                })
            }
            Err(err) => {
                let msg = match engine.detect_lang() {
                    Lang::ZhCn => format!("锁屏失败: {err}"),
                    Lang::En => format!("Failed to lock screen: {err}"),
                };
                json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": msg }]
                })
            }
        },
        other => json!({
            "isError": true,
            "content": [{ "type": "text", "text": format!("Unknown tool: {other}") }]
        }),
    }
}
