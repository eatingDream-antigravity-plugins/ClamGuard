use crate::i18n::{detect_lang, parse_lang_tag, Lang};
use crate::mcp::telemetry::{summarize_for_model, TelemetryEngine};
use crate::platform::display_ffi::{clamp_percent, DisplayBackend};
use crate::platform::macos::ProbeBackend;
use crate::state::SettingsPatch;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

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
                "name": "Mac Clamshell Dashboard",
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

pub fn list_tools_json() -> Value {
    json!({
        "tools": [
            {
                "name": "clamshell_panel",
                "title": "Mac Clamshell",
                "description": "Open the Mac Clamshell live dashboard showing macOS lid state, power/battery, thermal pressure, built-in display brightness, display guard status, CPU/memory load, and active throttling profile.",
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
                            { "type": "global", "title": "Mac Clamshell" },
                            { "type": "aux_pane", "title": "Mac Clamshell" }
                        ]
                    }
                }
            },
            {
                "name": "clamshell_refresh",
                "title": "Refresh Clamshell Telemetry",
                "description": "Fetch fresh macOS lid, power, thermal pressure, display brightness, guard status, and policy telemetry. Pass ifHash to receive a minimal { unchanged: true } payload when state has not changed.",
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
                "description": "Adjust MacBook built-in display brightness (0-100%) immediately when awake and synchronize the wake-restore target. While lid is closed, updates the saved restore target safely without lighting the closed panel. Returns an error on desktop Macs without a built-in display.",
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
                "description": "Lock the macOS screen immediately without putting the Mac to sleep, keeping background builds and AI agents running. Uses native SACLockScreenImmediate with automatic CGSession/pmset fallback and zero Accessibility permissions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        ]
    })
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
            let if_hash = args
                .get("ifHash")
                .or_else(|| args.get("if_hash"))
                .and_then(|v| v.as_str());
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false)
                || args.get("fresh").and_then(|v| v.as_bool()).unwrap_or(false);
            let (unchanged, sc, snap) = engine.refresh_json(if_hash, force);
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
                Ok(collected) => {
                    let mut sc =
                        serde_json::to_value(&*collected.snapshot).unwrap_or_else(|_| json!({}));
                    if let Some(obj) = sc.as_object_mut() {
                        obj.insert("unchanged".into(), json!(false));
                        obj.insert("serverMs".into(), json!(collected.server_ms));
                        obj.insert("fromCache".into(), json!(collected.from_cache));
                    }
                    json!({
                        "content": [{ "type": "text", "text": summarize_for_model(&collected.snapshot) }],
                        "structuredContent": sc
                    })
                }
                Err(err) => {
                    let msg = match detect_lang(None) {
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
            let Some(raw_pct) = args.get("percent").and_then(|v| v.as_f64()) else {
                let msg = match detect_lang(None) {
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
            let lid_override = std::env::var("MAC_CLAMSHELL_MOCK_LID")
                .ok()
                .and_then(|v| match v.trim() {
                    "closed" => Some(true),
                    "open" => Some(false),
                    _ => None,
                });
            match engine.set_brightness(clamped as f64, lid_override) {
                Ok((collected, action)) => {
                    if action.mode == "unavailable" {
                        return json!({
                            "isError": true,
                            "content": [{ "type": "text", "text": "Brightness update failed: no built-in display detected on this Mac" }]
                        });
                    }
                    let lang = parse_lang_tag(&collected.snapshot.locale).unwrap_or(Lang::En);
                    let mut sc =
                        serde_json::to_value(&*collected.snapshot).unwrap_or_else(|_| json!({}));
                    if let Some(obj) = sc.as_object_mut() {
                        obj.insert("unchanged".into(), json!(false));
                        obj.insert("serverMs".into(), json!(collected.server_ms));
                        obj.insert("fromCache".into(), json!(collected.from_cache));
                        obj.insert(
                            "brightnessAction".into(),
                            serde_json::to_value(&action).unwrap_or_else(|_| json!({})),
                        );
                    }
                    let note = match (lang, action.mode.as_str()) {
                        (Lang::ZhCn, "saved_target") => format!(
                            "已保存开盖恢复亮度目标: {}%",
                            action.saved_percent
                        ),
                        (Lang::ZhCn, _) => format!(
                            "内建屏幕亮度已设为 {}%",
                            action.percent.unwrap_or(clamped)
                        ),
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
                    let msg = match detect_lang(None) {
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
            Ok((method, locked_at_ms, server_ms)) => {
                let text = match engine.detect_lang() {
                    Lang::ZhCn => format!("已锁屏（{method}），Mac 将保持唤醒以继续运行后台任务。"),
                    Lang::En => format!(
                        "Screen locked ({method}); Mac remains awake for background tasks."
                    ),
                };
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "structuredContent": {
                        "ok": true,
                        "method": method,
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
