use crate::guard::DisplayGuardSummary;
use crate::platform::macos::run_cmd_timeout;
use crate::state::{
    abbreviate_home, atomic_write, ensure_private_dir, load_guard_state_opt, read_settings,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const LAUNCHAGENT_LABEL: &str = "dev.mac-clamshell-mcp.display-guard";
pub const DEFAULT_GUARD_INTERVAL_SECS: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardServiceStatus {
    pub label: String,
    pub guard_enabled: bool,
    pub plist_installed: bool,
    pub plist_path: String,
    pub loaded: bool,
    pub interval_secs: Option<u64>,
    pub binary_path: Option<String>,
    pub last_state: Option<DisplayGuardSummary>,
}

pub fn launchagents_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("MAC_CLAMSHELL_LAUNCHAGENTS_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join("Library").join("LaunchAgents");
        }
    }
    std::env::temp_dir().join("LaunchAgents")
}

pub fn plist_path() -> PathBuf {
    launchagents_dir().join(format!("{LAUNCHAGENT_LABEL}.plist"))
}

pub fn plist_exists() -> bool {
    plist_path().is_file()
}

fn xml_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn parse_plist_fields(xml: &str) -> (Option<u64>, Option<String>) {
    let mut interval = None;
    let mut bin_path = None;
    let lines: Vec<&str> = xml.lines().map(str::trim).collect();
    for (idx, line) in lines.iter().enumerate() {
        if *line == "<key>StartInterval</key>" {
            if let Some(next) = lines.get(idx + 1) {
                if let Some(val) = next
                    .strip_prefix("<integer>")
                    .and_then(|s| s.strip_suffix("</integer>"))
                {
                    interval = val.trim().parse::<u64>().ok();
                }
            }
        }
        if *line == "<key>ProgramArguments</key>" {
            for sub in lines.iter().skip(idx + 1).take(6) {
                if let Some(val) = sub
                    .strip_prefix("<string>")
                    .and_then(|s| s.strip_suffix("</string>"))
                {
                    bin_path = Some(val.to_string());
                    break;
                }
            }
        }
    }
    (interval, bin_path)
}

fn skip_launchctl() -> bool {
    std::env::var("MAC_CLAMSHELL_SKIP_LAUNCHCTL")
        .ok()
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
        || !cfg!(target_os = "macos")
}

fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn is_launchctl_loaded() -> bool {
    if skip_launchctl() {
        return plist_exists();
    }
    let target = format!("gui/{}/{LAUNCHAGENT_LABEL}", current_uid());
    let (ok, _) = run_cmd_timeout("/bin/launchctl", &["print", &target], 1500);
    ok
}

pub fn is_launchagent_active() -> bool {
    plist_exists() && is_launchctl_loaded()
}

pub fn guard_service_status(data_dir: &Path) -> GuardServiceStatus {
    let settings = read_settings(data_dir);
    let p_path = plist_path();
    let plist_installed = p_path.is_file();
    let (interval_secs, binary_path) = if plist_installed {
        fs::read_to_string(&p_path)
            .ok()
            .map(|s| parse_plist_fields(&s))
            .unwrap_or((None, None))
    } else {
        (None, None)
    };
    let loaded = plist_installed && is_launchctl_loaded();
    let last_state = load_guard_state_opt(data_dir).as_ref().map(DisplayGuardSummary::from);

    GuardServiceStatus {
        label: LAUNCHAGENT_LABEL.to_string(),
        guard_enabled: settings.guard_enabled,
        plist_installed,
        plist_path: abbreviate_home(&p_path),
        loaded,
        interval_secs,
        binary_path: binary_path.map(|p| abbreviate_home(Path::new(&p))),
        last_state,
    }
}

fn resolve_guard_binary(bin_override: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(b) = bin_override {
        if b.is_file() {
            return fs::canonicalize(b).or_else(|_| Ok(b.to_path_buf()));
        }
        return Err(format!("executable not found at {}", b.display()));
    }
    if let Ok(exe) = std::env::current_exe() {
        if exe.is_file() {
            return Ok(exe);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let plugin_bin = PathBuf::from(home)
            .join(".gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp");
        if plugin_bin.is_file() {
            return Ok(plugin_bin);
        }
    }
    Err("unable to locate mac-clamshell-mcp binary (pass --bin <PATH>)".to_string())
}

pub fn render_launchagent_plist(bin_path: &Path, data_dir: &Path, interval_secs: u64) -> String {
    let log_dir = data_dir.join("logs");
    let bin_esc = xml_escape(&bin_path.to_string_lossy());
    let data_esc = xml_escape(&data_dir.to_string_lossy());
    let out_esc = xml_escape(&log_dir.join("display-guard.log").to_string_lossy());
    let err_esc = xml_escape(&log_dir.join("display-guard.err.log").to_string_lossy());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHAGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin_esc}</string>
    <string>watch</string>
    <string>--data-dir</string>
    <string>{data_esc}</string>
  </array>
  <key>StartInterval</key>
  <integer>{interval_secs}</integer>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>LowPriorityBackgroundIO</key>
  <true/>
  <key>Nice</key>
  <integer>15</integer>
  <key>StandardOutPath</key>
  <string>{out_esc}</string>
  <key>StandardErrorPath</key>
  <string>{err_esc}</string>
</dict>
</plist>
"#
    )
}

pub fn install_launchagent(
    data_dir: &Path,
    bin_override: Option<&Path>,
    interval_secs: u64,
) -> Result<GuardServiceStatus, String> {
    if interval_secs < 2 {
        return Err("--interval must be >= 2 seconds".to_string());
    }
    let bin_path = resolve_guard_binary(bin_override)?;
    ensure_private_dir(data_dir).map_err(|e| format!("failed to create data dir: {e}"))?;
    let log_dir = data_dir.join("logs");
    ensure_private_dir(&log_dir).map_err(|e| format!("failed to create log dir: {e}"))?;

    let agents_dir = launchagents_dir();
    fs::create_dir_all(&agents_dir)
        .map_err(|e| format!("failed to create {}: {e}", agents_dir.display()))?;

    let p_path = plist_path();
    let xml = render_launchagent_plist(&bin_path, data_dir, interval_secs);
    atomic_write(&p_path, xml.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", p_path.display()))?;

    #[cfg(target_os = "macos")]
    {
        let p_str = p_path.to_string_lossy().into_owned();
        let (lint_ok, lint_out) = run_cmd_timeout("/usr/bin/plutil", &["-lint", &p_str], 2000);
        if !lint_ok {
            let _ = fs::remove_file(&p_path);
            return Err(format!("generated plist failed plutil validation: {}", lint_out.trim()));
        }
    }

    if !skip_launchctl() {
        let uid = current_uid();
        let gui_domain = format!("gui/{uid}");
        let service_target = format!("gui/{uid}/{LAUNCHAGENT_LABEL}");
        let p_str = p_path.to_string_lossy().into_owned();
        let _ = run_cmd_timeout("/bin/launchctl", &["bootout", &service_target], 2000);
        let (boot_ok, _) =
            run_cmd_timeout("/bin/launchctl", &["bootstrap", &gui_domain, &p_str], 3000);
        if !boot_ok {
            return Err(format!(
                "launchctl bootstrap failed for {} (plist written at {})",
                service_target,
                p_path.display()
            ));
        }
        let _ = run_cmd_timeout("/bin/launchctl", &["kickstart", "-k", &service_target], 2000);
    }

    Ok(guard_service_status(data_dir))
}

pub fn uninstall_launchagent(data_dir: &Path) -> Result<GuardServiceStatus, String> {
    let p_path = plist_path();
    if !skip_launchctl() {
        let service_target = format!("gui/{}/{LAUNCHAGENT_LABEL}", current_uid());
        let _ = run_cmd_timeout("/bin/launchctl", &["bootout", &service_target], 2000);
    }
    if p_path.exists() {
        fs::remove_file(&p_path)
            .map_err(|e| format!("failed to remove {}: {e}", p_path.display()))?;
    }
    Ok(guard_service_status(data_dir))
}
