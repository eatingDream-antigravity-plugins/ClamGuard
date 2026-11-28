#[cfg(all(target_os = "macos", not(test)))]
use crate::platform::macos::run_cmd_exit_code;
#[cfg(target_os = "macos")]
use crate::platform::macos::run_cmd_timeout;
use crate::state::{
    abbreviate_home, atomic_write_in_owned_dir, ensure_owned_dir, ensure_private_dir,
    load_guard_state_opt, read_bounded_regular_file, read_settings, DisplayGuardSummary,
    MAX_PLIST_FILE_BYTES,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootout_warning: Option<String>,
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
    let uid = unsafe { libc::geteuid() };
    std::env::temp_dir().join(format!("LaunchAgents-{uid}"))
}

pub fn plist_path() -> PathBuf {
    launchagents_dir().join(format!("{LAUNCHAGENT_LABEL}.plist"))
}

pub fn plist_exists() -> bool {
    fs::symlink_metadata(plist_path())
        .is_ok_and(|meta| !meta.file_type().is_symlink() && meta.is_file())
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
    #[cfg(any(test, feature = "test-mocks"))]
    if std::env::var("MAC_CLAMSHELL_SKIP_LAUNCHCTL")
        .ok()
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
    {
        return true;
    }
    !cfg!(target_os = "macos")
}

#[cfg(all(target_os = "macos", not(test)))]
fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn is_launchctl_loaded() -> bool {
    #[cfg(any(test, feature = "test-mocks"))]
    if let Ok(mock) = std::env::var("MAC_CLAMSHELL_MOCK_LAUNCHCTL") {
        return matches!(
            mock.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "loaded"
        );
    }
    if skip_launchctl() {
        return plist_exists();
    }
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let target = format!("gui/{}/{LAUNCHAGENT_LABEL}", current_uid());
        let (ok, _) = run_cmd_timeout("/bin/launchctl", &["print", &target], 1500);
        ok
    }
    #[cfg(any(not(target_os = "macos"), test))]
    {
        false
    }
}

pub fn is_launchagent_active() -> bool {
    plist_exists() && is_launchctl_loaded()
}

pub fn guard_service_status(data_dir: &Path) -> GuardServiceStatus {
    let settings = read_settings(data_dir);
    let p_path = plist_path();
    let plist_installed = fs::symlink_metadata(&p_path)
        .is_ok_and(|meta| !meta.file_type().is_symlink() && meta.is_file());
    let (interval_secs, binary_path) = if plist_installed {
        read_bounded_regular_file(&p_path, MAX_PLIST_FILE_BYTES)
            .ok()
            .map(|s| parse_plist_fields(&s))
            .unwrap_or((None, None))
    } else {
        (None, None)
    };
    let loaded = plist_installed && is_launchctl_loaded();
    let last_state = load_guard_state_opt(data_dir)
        .as_ref()
        .map(DisplayGuardSummary::from);

    GuardServiceStatus {
        label: LAUNCHAGENT_LABEL.to_string(),
        guard_enabled: settings.guard_enabled,
        plist_installed,
        plist_path: abbreviate_home(&p_path),
        loaded,
        interval_secs,
        binary_path: binary_path.map(|p| abbreviate_home(Path::new(&p))),
        last_state,
        bootout_warning: None,
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
    #[cfg(test)]
    if !skip_launchctl() {
        let _ = (data_dir, bin_override);
        return Err("refusing live launchctl install under #[cfg(test)]".to_string());
    }
    let bin_path = resolve_guard_binary(bin_override)?;
    ensure_private_dir(data_dir).map_err(|e| format!("failed to create data dir: {e}"))?;
    let log_dir = data_dir.join("logs");
    ensure_private_dir(&log_dir).map_err(|e| format!("failed to create log dir: {e}"))?;

    let agents_dir = launchagents_dir();
    ensure_owned_dir(&agents_dir)
        .map_err(|e| format!("failed to prepare {}: {e}", agents_dir.display()))?;

    let p_path = plist_path();
    let xml = render_launchagent_plist(&bin_path, data_dir, interval_secs);
    atomic_write_in_owned_dir(&p_path, xml.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", p_path.display()))?;

    #[cfg(target_os = "macos")]
    {
        let p_str = p_path.to_string_lossy().into_owned();
        let (lint_ok, lint_out) = run_cmd_timeout("/usr/bin/plutil", &["-lint", &p_str], 2000);
        if !lint_ok {
            let _ = fs::remove_file(&p_path);
            return Err(format!(
                "generated plist failed plutil validation: {}",
                lint_out.trim()
            ));
        }
    }

    #[cfg(all(target_os = "macos", not(test)))]
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
        let _ = run_cmd_timeout(
            "/bin/launchctl",
            &["kickstart", "-k", &service_target],
            2000,
        );
    }

    Ok(guard_service_status(data_dir))
}

pub fn uninstall_launchagent(data_dir: &Path) -> Result<GuardServiceStatus, String> {
    #[cfg(test)]
    if !skip_launchctl() {
        let _ = data_dir;
        return Err("refusing live launchctl uninstall under #[cfg(test)]".to_string());
    }
    let p_path = plist_path();
    let bootout_warning = if skip_launchctl() {
        remove_plist(&p_path)?;
        None
    } else {
        #[cfg(all(target_os = "macos", not(test)))]
        {
            uninstall_verified(&p_path, current_uid(), |args, timeout| {
                run_cmd_exit_code("/bin/launchctl", args, timeout).0
            })?
        }
        #[cfg(any(not(target_os = "macos"), test))]
        {
            return Err("refusing live launchctl uninstall under #[cfg(test)]".to_string());
        }
    };
    let mut status = guard_service_status(data_dir);
    status.bootout_warning = bootout_warning;
    Ok(status)
}

fn remove_plist(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to remove {}: {e}", path.display())),
    }
}

fn uninstall_verified(
    path: &Path,
    uid: u32,
    mut launchctl: impl FnMut(&[&str], u64) -> Option<i32>,
) -> Result<Option<String>, String> {
    let target = format!("gui/{uid}/{LAUNCHAGENT_LABEL}");
    let bootout = launchctl(&["bootout", &target], 2000);
    let removal = remove_plist(path);
    let print = launchctl(&["print", &target], 1500);
    let describe = |code: Option<i32>| match code {
        Some(code) => format!("exit code {code}"),
        None => "no exit status".to_string(),
    };
    let verification = match print {
        Some(0) => Err(format!(
            "LaunchAgent {target} is still loaded after uninstall (launchctl bootout: {})",
            describe(bootout)
        )),
        // launchctl reports 113 when the specified service cannot be found.
        Some(113) => Ok(()),
        _ => Err(format!(
            "could not verify that LaunchAgent {target} was unloaded (launchctl bootout: {}; launchctl print: {})",
            describe(bootout),
            describe(print)
        )),
    };
    match (removal, verification) {
        (Err(remove), Err(verify)) => Err(format!("{remove}. {verify}")),
        (Err(err), _) | (_, Err(err)) => Err(err),
        (Ok(()), Ok(())) => Ok((bootout != Some(0)).then(|| {
            format!(
                "launchctl bootout returned {}. launchctl print confirmed {target} is absent.",
                describe(bootout)
            )
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::atomic_write;

    #[test]
    fn uninstall_verifies_launchd_even_on_bootout_failure() {
        let dir =
            std::env::temp_dir().join(format!("clamshell-launchctl-test-{}", std::process::id()));
        let plist = dir.join("agent.plist");
        for (bootout, print) in [
            (Some(0), Some(113)),
            (Some(5), Some(113)),
            (Some(5), Some(0)),
            (Some(0), Some(0)),
            (None, None),
        ] {
            atomic_write(&plist, b"plist").unwrap();
            let mut methods = Vec::new();
            let result = uninstall_verified(&plist, 123, |args, _| {
                assert_eq!(args[1], format!("gui/123/{LAUNCHAGENT_LABEL}"));
                methods.push(args[0].to_string());
                if args[0] == "bootout" {
                    assert!(plist.is_file());
                    bootout
                } else {
                    assert!(!plist.exists(), "print must run after plist removal");
                    print
                }
            });
            assert_eq!(methods, ["bootout", "print"]);
            match (bootout, print) {
                (Some(0), Some(113)) => assert_eq!(result.unwrap(), None),
                (Some(5), Some(113)) => {
                    let warning = result.unwrap().unwrap();
                    assert!(warning.contains("exit code 5"));
                    assert!(warning.contains("confirmed"));
                }
                (_, Some(0)) => {
                    let err = result.unwrap_err();
                    assert!(err.contains("still loaded"));
                    assert!(err.contains(&format!("exit code {}", bootout.unwrap())));
                }
                _ => assert!(result.unwrap_err().contains("could not verify")),
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn install_and_uninstall_refuse_live_launchctl_under_cfg_test() {
        let dir =
            std::env::temp_dir().join(format!("clamshell-launchctl-guard-{}", std::process::id()));
        if !skip_launchctl() {
            let install_err = install_launchagent(&dir, None, 10).unwrap_err();
            assert!(install_err.contains("#[cfg(test)]"));
            let uninstall_err = uninstall_launchagent(&dir).unwrap_err();
            assert!(uninstall_err.contains("#[cfg(test)]"));
        }
    }

    #[test]
    fn plist_write_preserves_launchagents_dir_mode_while_writing_0600_plist() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("clamshell-plist-perms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let agents = dir.join("LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        fs::set_permissions(&agents, fs::Permissions::from_mode(0o755)).unwrap();

        let p_path = agents.join(format!("{LAUNCHAGENT_LABEL}.plist"));
        let xml = render_launchagent_plist(Path::new("/usr/bin/true"), &dir, 10);
        atomic_write_in_owned_dir(&p_path, xml.as_bytes()).unwrap();

        assert_eq!(
            fs::metadata(&agents).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&p_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
