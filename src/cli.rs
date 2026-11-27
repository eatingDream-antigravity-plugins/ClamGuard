use crate::guard::{read_display_info, run_watch_daemon, run_watch_once, set_brightness_synced};
use crate::hook::run_hook;
use crate::i18n::{detect_lang, parse_lang_tag, Lang};
use crate::launchagent::{
    guard_service_status, install_launchagent, uninstall_launchagent, DEFAULT_GUARD_INTERVAL_SECS,
};
use crate::mcp::server::serve_stdio;
use crate::mcp::telemetry::{format_fan, TelemetryEngine};
use crate::platform::display_ffi::{DisplayBackend, MacOsDisplayBackend};
use crate::platform::fan::MacOsFanSensorSource;
use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use crate::policy::{format_lid, format_power, format_profile, format_thermal, format_topology};
use crate::runner::{execute_run, RunOptions};
use crate::state::{load_guard_state, now_epoch_ms, read_settings, resolve_data_dir};
use serde_json::json;
use std::path::PathBuf;

const USAGE_EN: &str = "\
Clam Guard — keeps your Mac cool and the screen dark with the lid closed.

USAGE:
  mac-clamshell-mcp <COMMAND> [OPTIONS]

COMMANDS:
  serve [--data-dir DIR] [--panel-html PATH]
      Run the MCP stdio JSON-RPC server (active lid guard + dashboard).

  status [--json] [--data-dir DIR]
      Inspect live macOS lid state, power, heat, built-in brightness, and active mode.

  brightness [get | set <0..100>] [--json] [--lid-state open|closed] [--data-dir DIR]
      Read or set built-in screen brightness (0-100%) in sync with lid guard wake-restore.

  lock [--json] [--data-dir DIR]
      Lock the macOS screen immediately without putting the Mac to sleep.

  guard <install | uninstall | status> [--interval SECS] [--bin PATH] [--json] [--data-dir DIR]
      Install, remove, or inspect the standalone 24/7 macOS LaunchAgent lid guard.

  run [--mode auto|normal|utility|eco|strict] [--jobs N] [--quiet|-q] [--data-dir DIR]
      (--sh \"<cmd>\" | [--] <cmd> [args...])
      Execute a command wrapped with live taskpolicy, nice, and concurrency env limits.

  hook <session-start | pre-invocation> [--data-dir DIR]
      Antigravity lifecycle hook (early-exit JSON stdin -> JSON stdout, always fail-open).

  watch [--daemon] [--interval SECS] [--data-dir DIR]
      Run one lid guard check (for LaunchAgent StartInterval) or loop as a foreground daemon.

  --version, -V    Print version
  --help, -h       Print help";

const USAGE_ZH: &str = "\
Clam Guard —— 合盖时让 Mac 保持凉爽、屏幕不亮。

用法：
  mac-clamshell-mcp <命令> [选项]

命令：
  serve [--data-dir DIR] [--panel-html PATH]
      启动 MCP stdio 服务（内置合盖熄屏保护与控制面板）。

  status [--json] [--data-dir DIR]
      查看当前机盖、电源、温度、内建屏幕亮度、守护状态与当前模式。

  brightness [get | set <0..100>] [--json] [--lid-state open|closed] [--data-dir DIR]
      读取或设置内建屏幕亮度（0-100%），合盖时仅保存唤醒恢复亮度。

  lock [--json] [--data-dir DIR]
      立即锁定 macOS 屏幕并保持 Mac 唤醒，不中断后台构建或 Agent 任务。

  guard <install | uninstall | status> [--interval SECS] [--bin PATH] [--json] [--data-dir DIR]
      安装、卸载或查看独立常驻的 macOS LaunchAgent 合盖熄屏守护。

  run [--mode auto|normal|utility|eco|strict] [--jobs N] [--quiet|-q] [--data-dir DIR]
      (--sh \"<cmd>\" | [--] <cmd> [args...])
      按当前状态使用 taskpolicy、nice 与并发环境变量运行命令。

  hook <session-start | pre-invocation> [--data-dir DIR]
      Antigravity 生命周期钩子（快速读取 JSON，失败默认放行）。

  watch [--daemon] [--interval SECS] [--data-dir DIR]
      单次执行或前台常驻运行合盖熄屏与高温降亮守护。

  --version, -V    显示版本号
  --help, -h       显示帮助信息";

fn usage_str() -> &'static str {
    match detect_lang(None) {
        Lang::ZhCn => USAGE_ZH,
        Lang::En => USAGE_EN,
    }
}

fn take_opt_val(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    if let Some(rest) = args[*i].strip_prefix(&format!("{flag}=")) {
        return Ok(rest.to_string());
    }
    if *i + 1 >= args.len() {
        return Err(format!("missing value for {flag}"));
    }
    *i += 1;
    Ok(args[*i].clone())
}

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{}", usage_str());
        return 0;
    }
    if matches!(args[0].as_str(), "-V" | "--version") {
        println!("mac-clamshell-mcp {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    match args[0].as_str() {
        "serve" => cmd_serve(&args[1..]),
        "status" => cmd_status(&args[1..]),
        "brightness" => cmd_brightness(&args[1..]),
        "lock" => cmd_lock(&args[1..]),
        "guard" => cmd_guard(&args[1..]),
        "run" => cmd_run(&args[1..]),
        "hook" => cmd_hook(&args[1..]),
        "watch" => cmd_watch(&args[1..]),
        other => {
            eprintln!(
                "mac-clamshell-mcp: unknown command \"{other}\"\n\n{}",
                usage_str()
            );
            2
        }
    }
}

fn cmd_serve(args: &[String]) -> i32 {
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut panel_html_opt: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => match take_opt_val(args, &mut i, "--data-dir") {
                Ok(v) => data_dir_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp serve: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--data-dir=") => {
                data_dir_opt = Some(PathBuf::from(&s["--data-dir=".len()..]));
            }
            "--panel-html" => match take_opt_val(args, &mut i, "--panel-html") {
                Ok(v) => panel_html_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp serve: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--panel-html=") => {
                panel_html_opt = Some(PathBuf::from(&s["--panel-html=".len()..]));
            }
            "-h" | "--help" => {
                println!("{}", usage_str());
                return 0;
            }
            _ => {}
        }
        i += 1;
    }

    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    if let Err(err) = serve_stdio(data_dir, panel_html_opt) {
        eprintln!("[mac-clamshell-mcp] fatal: {err}");
        return 1;
    }
    0
}

fn cmd_status(args: &[String]) -> i32 {
    let mut json_mode = false;
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json_mode = true,
            "--data-dir" => match take_opt_val(args, &mut i, "--data-dir") {
                Ok(v) => data_dir_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp status: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--data-dir=") => {
                data_dir_opt = Some(PathBuf::from(&s["--data-dir=".len()..]));
            }
            "-h" | "--help" => {
                println!("{}", usage_str());
                return 0;
            }
            other => {
                eprintln!("mac-clamshell-mcp status: unknown option \"{other}\"");
                return 2;
            }
        }
        i += 1;
    }

    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "mac-clamshell-mcp".to_string());
    let engine = TelemetryEngine::new(
        data_dir,
        binary_path,
        MacOsProbeBackend::default(),
        MacOsDisplayBackend,
        MacOsFanSensorSource::default(),
    );
    let collected = engine.collect(true);
    let s = &collected.snapshot;

    if json_mode {
        match serde_json::to_string_pretty(s.as_ref()) {
            Ok(out) => println!("{out}"),
            Err(e) => {
                eprintln!("mac-clamshell-mcp status: serialize error: {e}");
                return 1;
            }
        }
        return 0;
    }

    let lang = parse_lang_tag(&s.locale).unwrap_or(Lang::En);
    let therm_str = format_thermal(
        &s.thermal_state,
        &s.thermal_pressure,
        s.cpu_speed_limit_pct,
        lang,
    );
    let topo_str = format_topology(&s.cpu_topology, lang);
    let prof_str = format_profile(&s.policy, &s.cpu_topology, lang);
    let tp = if s.policy.taskpolicy.is_empty() {
        "none"
    } else {
        s.policy.taskpolicy.as_str()
    };

    match lang {
        Lang::ZhCn => {
            let disp_str = if s.display.available {
                if s.display.lid_closed || s.display.mode == "saved_target" {
                    let ext = if s.display.external_count > 0 {
                        format!("，外接 {} 台", s.display.external_count)
                    } else {
                        String::new()
                    };
                    let panel_state = if s.display.internal_offline {
                        "屏幕已关"
                    } else if s.display.confirmed_asleep {
                        "屏幕已熄"
                    } else {
                        "屏幕亮着"
                    };
                    format!(
                        "已合盖（{panel_state}{ext}），唤醒后恢复 {}%",
                        s.display.saved_percent.unwrap_or(20)
                    )
                } else if s.display.confirmed_asleep {
                    format!(
                        "屏幕已熄，唤醒后恢复 {}%",
                        s.display.saved_percent.or(s.display.percent).unwrap_or(20)
                    )
                } else {
                    format!("{}%（亮着）", s.display.percent.unwrap_or(0))
                }
            } else {
                "没有内建屏幕".to_string()
            };
            let lpm_zh = match s.low_power_mode.as_str() {
                "1" => "开",
                "0" => "关",
                _ => "未知",
            };
            let guard_zh = match s.guard_status.as_str() {
                "protecting" => "保护中",
                "thermal_cap" => "屏幕：高温降亮",
                "armed" => "后台运行中",
                "paused" => "已暂停",
                _ => "空闲（在 Antigravity 打开时自动启用）",
            };
            let src_zh = if s.policy.mode_source == "manual" {
                "手动"
            } else {
                "自动"
            };

            println!("Clam Guard 状态");
            println!("  机盖状态   : {}", format_lid(&s.lid_state, lang));
            println!(
                "  电源       : {}",
                format_power(&s.power_source, s.battery_pct, lang)
            );
            println!("  温度       : {therm_str}");
            println!("  风扇       : {}", format_fan(&s.fan, lang));
            println!("  低电量模式 : {lpm_zh}");
            println!("  内建屏幕   : {disp_str}");
            println!("  合盖守护   : {guard_zh}");
            println!("  处理器     : {topo_str}");
            println!(
                "  当前模式   : {} [{src_zh}] — {prof_str}",
                s.policy.profile
            );
            println!(
                "  调度参数   : {tp} (nice={}, 最多 {} 个任务)",
                s.policy.nice_level, s.policy.max_jobs
            );
        }
        Lang::En => {
            let disp_str = if s.display.available {
                if s.display.lid_closed || s.display.mode == "saved_target" {
                    let ext = if s.display.external_count > 0 {
                        format!(", {} external", s.display.external_count)
                    } else {
                        String::new()
                    };
                    let panel_state = if s.display.internal_offline {
                        "screen off"
                    } else if s.display.confirmed_asleep {
                        "screen asleep"
                    } else {
                        "screen on"
                    };
                    format!(
                        "closed ({panel_state}{ext}), wakes at {}%",
                        s.display.saved_percent.unwrap_or(20)
                    )
                } else if s.display.confirmed_asleep {
                    format!(
                        "screen asleep, wakes at {}%",
                        s.display.saved_percent.or(s.display.percent).unwrap_or(20)
                    )
                } else {
                    format!("{}% (screen on)", s.display.percent.unwrap_or(0))
                }
            } else {
                "no built-in screen".to_string()
            };
            let lpm_en = match s.low_power_mode.as_str() {
                "1" => "on",
                "0" => "off",
                _ => "unknown",
            };
            let guard_en = match s.guard_status.as_str() {
                "protecting" => "protecting",
                "thermal_cap" => "Screen: dimmed for heat",
                "armed" => "on",
                "paused" => "paused",
                _ => "idle",
            };

            println!("Clam Guard Status");
            println!("  Lid State      : {}", format_lid(&s.lid_state, lang));
            println!(
                "  Power Source   : {}",
                format_power(&s.power_source, s.battery_pct, lang)
            );
            println!("  Heat           : {therm_str}");
            println!("  Fans           : {}", format_fan(&s.fan, lang));
            println!("  Low Power Mode : {lpm_en}");
            println!("  Screen         : {disp_str}");
            println!("  Lid Guard      : {guard_en}");
            println!("  CPU            : {topo_str}");
            println!(
                "  Active Mode    : {} [{}] — {prof_str}",
                s.policy.profile, s.policy.mode_source
            );
            println!(
                "  Throttle Cmd   : {tp} (nice={}, maxJobs={})",
                s.policy.nice_level, s.policy.max_jobs
            );
        }
    }
    0
}

fn cmd_brightness(args: &[String]) -> i32 {
    let mut subcmd = "get";
    let mut target_val: Option<String> = None;
    let mut json_mode = false;
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut lid_override: Option<bool> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "get" => subcmd = "get",
            "set" => {
                subcmd = "set";
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                    target_val = Some(args[i].clone());
                }
            }
            "--json" => json_mode = true,
            "--data-dir" => match take_opt_val(args, &mut i, "--data-dir") {
                Ok(v) => data_dir_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp brightness: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--data-dir=") => {
                data_dir_opt = Some(PathBuf::from(&s["--data-dir=".len()..]));
            }
            "--lid-state" => match take_opt_val(args, &mut i, "--lid-state") {
                Ok(v) => match v.as_str() {
                    "closed" => lid_override = Some(true),
                    "open" => lid_override = Some(false),
                    _ => {
                        eprintln!(
                            "mac-clamshell-mcp brightness: --lid-state must be open or closed"
                        );
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("mac-clamshell-mcp brightness: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("{}", usage_str());
                return 0;
            }
            other if subcmd == "set" && target_val.is_none() => {
                target_val = Some(other.to_string());
            }
            other => {
                eprintln!("mac-clamshell-mcp brightness: unknown argument \"{other}\"");
                return 2;
            }
        }
        i += 1;
    }

    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    let settings = read_settings(&data_dir);
    let lang = detect_lang(settings.lang.as_deref());
    let probe = MacOsProbeBackend::default();
    let display = MacOsDisplayBackend;

    if subcmd == "get" {
        let hw_closed = probe.lid_closed_fast();
        let lid_str = match lid_override {
            Some(true) => "closed",
            Some(false) | None => match hw_closed {
                Some(true) => "closed",
                Some(false) => "open",
                None => "unknown",
            },
        };
        let state = load_guard_state(&data_dir);
        let info = read_display_info(&display, lid_str, &state);
        if json_mode {
            println!(
                "{}",
                serde_json::to_string(&info).unwrap_or_else(|_| "{}".into())
            );
        } else if info.available {
            if info.lid_closed || info.mode == "saved_target" {
                let sv = info.saved_percent.unwrap_or(20);
                let state_note = match (lang, info.internal_offline, info.confirmed_asleep) {
                    (Lang::ZhCn, true, _) => "内屏已离线",
                    (Lang::ZhCn, false, true) => "已确认熄屏",
                    (Lang::ZhCn, false, false) => "内屏仍在线",
                    (Lang::En, true, _) => "internal offline",
                    (Lang::En, false, true) => "confirmed asleep",
                    (Lang::En, false, false) => "internal online",
                };
                match lang {
                    Lang::ZhCn => println!("已合盖（{state_note}），恢复目标 {sv}%"),
                    Lang::En => println!("closed ({state_note}), saved target {sv}%"),
                }
            } else if info.confirmed_asleep {
                let sv = info.saved_percent.or(info.percent).unwrap_or(20);
                match lang {
                    Lang::ZhCn => println!("已熄屏，恢复目标 {sv}%"),
                    Lang::En => println!("asleep, saved target {sv}%"),
                }
            } else {
                let p = info.percent.unwrap_or(0);
                let sv = info.saved_percent.unwrap_or(p);
                match lang {
                    Lang::ZhCn => println!("{p}%（点亮，恢复目标={sv}%）"),
                    Lang::En => println!("{p}% (awake, saved={sv}%, mode={})", info.mode),
                }
            }
        } else {
            match lang {
                Lang::ZhCn => println!("未检测到内建屏幕"),
                Lang::En => println!("unavailable"),
            }
        }
        return 0;
    }

    let Some(raw) = target_val else {
        eprintln!("mac-clamshell-mcp brightness set: missing <0..100> percent");
        return 2;
    };
    let Ok(val) = raw.trim().trim_end_matches('%').parse::<f64>() else {
        eprintln!("mac-clamshell-mcp brightness set: invalid percent \"{raw}\"");
        return 2;
    };

    match set_brightness_synced(&data_dir, val, lid_override, &probe, &display) {
        Ok(action) => {
            if action.mode == "unavailable" {
                let err_msg = "no built-in display detected on this Mac";
                if json_mode {
                    println!("{}", json!({"error": err_msg}));
                } else {
                    eprintln!("mac-clamshell-mcp brightness set: {err_msg}");
                }
                return 1;
            }
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string(&action).unwrap_or_else(|_| "{}".into())
                );
            } else if action.mode == "saved_target" {
                match lang {
                    Lang::ZhCn => {
                        println!("屏幕唤醒后亮度恢复到 {}%（下次生效）", action.saved_percent)
                    }
                    Lang::En => println!(
                        "Brightness will go back to {}% when the screen wakes (applies next time)",
                        action.saved_percent
                    ),
                }
            } else {
                let p = action.percent.unwrap_or(action.requested_percent);
                match lang {
                    Lang::ZhCn => println!("亮度已设为 {p}%"),
                    Lang::En => println!("Brightness set to {p}%"),
                }
            }
            0
        }
        Err(e) => {
            if json_mode {
                println!("{}", json!({"error": e}));
            } else {
                eprintln!("mac-clamshell-mcp brightness set: {e}");
            }
            1
        }
    }
}

fn cmd_lock(args: &[String]) -> i32 {
    let mut json_mode = false;
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json_mode = true,
            "--data-dir" => match take_opt_val(args, &mut i, "--data-dir") {
                Ok(v) => data_dir_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp lock: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--data-dir=") => {
                data_dir_opt = Some(PathBuf::from(&s["--data-dir=".len()..]));
            }
            "-h" | "--help" => {
                println!("{}", usage_str());
                return 0;
            }
            other => {
                eprintln!("mac-clamshell-mcp lock: unknown option \"{other}\"");
                return 2;
            }
        }
        i += 1;
    }

    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    let settings = read_settings(&data_dir);
    let lang = detect_lang(settings.lang.as_deref());
    let display = MacOsDisplayBackend;

    match display.lock_screen_now() {
        Ok(outcome) => {
            let method = outcome.method;
            let verified = outcome.verified;
            let status = if verified { "verified" } else { "submitted" };
            let locked_at_ms = now_epoch_ms() as u64;
            if json_mode {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "method": method,
                        "verified": verified,
                        "status": status,
                        "locked_at_ms": locked_at_ms
                    })
                );
            } else {
                match (lang, verified) {
                    (Lang::ZhCn, true) => println!("已锁屏（{method}，已核验），Mac 将保持唤醒"),
                    (Lang::ZhCn, false) => {
                        println!("已提交锁屏请求（{method}，未核验会话锁），Mac 将保持唤醒")
                    }
                    (Lang::En, true) => {
                        println!("screen locked ({method}, verified); Mac remains awake")
                    }
                    (Lang::En, false) => {
                        println!(
                            "lock screen request submitted ({method}, session lock not verified); Mac remains awake"
                        )
                    }
                }
            }
            0
        }
        Err(e) => {
            if json_mode {
                println!("{}", json!({"ok": false, "error": e}));
            } else {
                match lang {
                    Lang::ZhCn => eprintln!("mac-clamshell-mcp lock: 锁屏失败: {e}"),
                    Lang::En => eprintln!("mac-clamshell-mcp lock: {e}"),
                }
            }
            1
        }
    }
}

fn cmd_guard(args: &[String]) -> i32 {
    let mut subcmd = "status";
    let mut interval_secs = DEFAULT_GUARD_INTERVAL_SECS;
    let mut bin_opt: Option<PathBuf> = None;
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut json_mode = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "install" => subcmd = "install",
            "uninstall" | "remove" => subcmd = "uninstall",
            "status" => subcmd = "status",
            "--json" => json_mode = true,
            "--interval" => match take_opt_val(args, &mut i, "--interval") {
                Ok(v) => match v.parse::<u64>() {
                    Ok(n) if n >= 2 => interval_secs = n,
                    _ => {
                        eprintln!("mac-clamshell-mcp guard: --interval must be >= 2 seconds");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("mac-clamshell-mcp guard: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--interval=") => match s["--interval=".len()..].parse::<u64>() {
                Ok(n) if n >= 2 => interval_secs = n,
                _ => {
                    eprintln!("mac-clamshell-mcp guard: --interval must be >= 2 seconds");
                    return 2;
                }
            },
            "--bin" => match take_opt_val(args, &mut i, "--bin") {
                Ok(v) => bin_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp guard: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--bin=") => {
                bin_opt = Some(PathBuf::from(&s["--bin=".len()..]));
            }
            "--data-dir" => match take_opt_val(args, &mut i, "--data-dir") {
                Ok(v) => data_dir_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp guard: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--data-dir=") => {
                data_dir_opt = Some(PathBuf::from(&s["--data-dir=".len()..]));
            }
            "-h" | "--help" => {
                println!("{}", usage_str());
                return 0;
            }
            other => {
                eprintln!("mac-clamshell-mcp guard: unknown argument \"{other}\"");
                return 2;
            }
        }
        i += 1;
    }

    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    let settings = read_settings(&data_dir);
    let lang = detect_lang(settings.lang.as_deref());

    match subcmd {
        "install" => match install_launchagent(&data_dir, bin_opt.as_deref(), interval_secs) {
            Ok(st) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&st).unwrap_or_else(|_| "{}".into())
                    );
                } else {
                    match lang {
                        Lang::ZhCn => println!(
                            "已安装并加载常驻守护 {}（每 {} 秒检查一次 -> {}）",
                            st.label,
                            st.interval_secs.unwrap_or(interval_secs),
                            st.plist_path
                        ),
                        Lang::En => println!(
                            "Installed and loaded LaunchAgent {} (every {}s -> {})",
                            st.label,
                            st.interval_secs.unwrap_or(interval_secs),
                            st.plist_path
                        ),
                    }
                }
                0
            }
            Err(e) => {
                if json_mode {
                    println!("{}", json!({"error": e}));
                } else {
                    eprintln!("mac-clamshell-mcp guard install: {e}");
                }
                1
            }
        },
        "uninstall" => match uninstall_launchagent(&data_dir) {
            Ok(st) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&st).unwrap_or_else(|_| "{}".into())
                    );
                } else {
                    if let Some(ref warning) = st.bootout_warning {
                        eprintln!("{warning}");
                    }
                    match lang {
                        Lang::ZhCn => println!("已卸载常驻守护 {}", st.label),
                        Lang::En => println!("Uninstalled LaunchAgent {}", st.label),
                    }
                }
                0
            }
            Err(e) => {
                if json_mode {
                    println!("{}", json!({"error": e}));
                } else {
                    eprintln!("mac-clamshell-mcp guard uninstall: {e}");
                }
                1
            }
        },
        _ => {
            let st = guard_service_status(&data_dir);
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&st).unwrap_or_else(|_| "{}".into())
                );
            } else {
                match lang {
                    Lang::ZhCn => {
                        println!("Clam Guard 合盖守护状态");
                        println!(
                            "  守护开关     : {}",
                            if st.guard_enabled {
                                "已启用"
                            } else {
                                "已暂停"
                            }
                        );
                        println!(
                            "  LaunchAgent  : {} ({})",
                            if st.loaded {
                                "后台运行中"
                            } else if st.plist_installed {
                                "已安装未加载"
                            } else {
                                "未安装（仅在 Antigravity 打开时生效）"
                            },
                            st.plist_path
                        );
                        if let Some(iv) = st.interval_secs {
                            println!("  检查间隔     : {iv} 秒");
                        }
                    }
                    Lang::En => {
                        println!("Clam Guard Lid Guard");
                        println!(
                            "  Guard Toggle : {}",
                            if st.guard_enabled {
                                "enabled"
                            } else {
                                "paused"
                            }
                        );
                        println!(
                            "  LaunchAgent  : {} ({})",
                            if st.loaded {
                                "loaded"
                            } else if st.plist_installed {
                                "installed (not loaded)"
                            } else {
                                "not installed (active while Antigravity MCP server runs)"
                            },
                            st.plist_path
                        );
                        if let Some(iv) = st.interval_secs {
                            println!("  Interval     : {iv}s");
                        }
                    }
                }
            }
            0
        }
    }
}

fn cmd_run(args: &[String]) -> i32 {
    let mut mode = "auto".to_string();
    let mut override_jobs: Option<u32> = None;
    let mut quiet = false;
    let mut sh_cmd: Option<String> = None;
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut cmd_args: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => match take_opt_val(args, &mut i, "--mode") {
                Ok(v) => mode = v,
                Err(e) => {
                    eprintln!("mac-clamshell-mcp run: {e}");
                    return 2;
                }
            },
            "--jobs" => match take_opt_val(args, &mut i, "--jobs") {
                Ok(v) => {
                    if v.is_empty() || v.starts_with('0') || !v.bytes().all(|b| b.is_ascii_digit())
                    {
                        eprintln!("mac-clamshell-mcp run: --jobs must be a positive integer");
                        return 2;
                    }
                    match v.parse::<u32>() {
                        Ok(n) if n > 0 => override_jobs = Some(n),
                        _ => {
                            eprintln!("mac-clamshell-mcp run: --jobs must be a positive integer");
                            return 2;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("mac-clamshell-mcp run: {e}");
                    return 2;
                }
            },
            "--quiet" | "-q" => quiet = true,
            "--data-dir" => match take_opt_val(args, &mut i, "--data-dir") {
                Ok(v) => data_dir_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp run: {e}");
                    return 2;
                }
            },
            "--sh" => match take_opt_val(args, &mut i, "--sh") {
                Ok(v) => {
                    sh_cmd = Some(v);
                    break;
                }
                Err(e) => {
                    eprintln!("mac-clamshell-mcp run: {e}");
                    return 2;
                }
            },
            "--" => {
                cmd_args.extend_from_slice(&args[i + 1..]);
                break;
            }
            "-h" | "--help" => {
                println!("{}", usage_str());
                return 0;
            }
            _ => {
                cmd_args.extend_from_slice(&args[i..]);
                break;
            }
        }
        i += 1;
    }

    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    match execute_run(
        &data_dir,
        RunOptions {
            mode,
            override_jobs,
            quiet,
            sh_cmd,
            cmd_args,
        },
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("mac-clamshell-mcp run: {e}");
            2
        }
    }
}

fn cmd_hook(args: &[String]) -> i32 {
    let mut event_arg: Option<String> = None;
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                if let Ok(v) = take_opt_val(args, &mut i, "--data-dir") {
                    data_dir_opt = Some(PathBuf::from(v));
                }
            }
            s if s.starts_with("--data-dir=") => {
                data_dir_opt = Some(PathBuf::from(&s["--data-dir=".len()..]));
            }
            other if !other.starts_with('-') && event_arg.is_none() => {
                event_arg = Some(other.to_string());
            }
            _ => {}
        }
        i += 1;
    }
    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    let output = run_hook(event_arg.as_deref(), &data_dir);
    println!(
        "{}",
        serde_json::to_string(&output).unwrap_or_else(|_| "{}".into())
    );
    0
}

fn cmd_watch(args: &[String]) -> i32 {
    let mut daemon = false;
    let mut interval_secs = 10u64;
    let mut data_dir_opt: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--daemon" => daemon = true,
            "--interval" => match take_opt_val(args, &mut i, "--interval") {
                Ok(v) => match v.parse::<u64>() {
                    Ok(n) if n >= 2 => interval_secs = n,
                    _ => {
                        eprintln!("mac-clamshell-mcp watch: --interval must be >= 2 seconds");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("mac-clamshell-mcp watch: {e}");
                    return 2;
                }
            },
            "--data-dir" => match take_opt_val(args, &mut i, "--data-dir") {
                Ok(v) => data_dir_opt = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("mac-clamshell-mcp watch: {e}");
                    return 2;
                }
            },
            s if s.starts_with("--data-dir=") => {
                data_dir_opt = Some(PathBuf::from(&s["--data-dir=".len()..]));
            }
            "-h" | "--help" => {
                println!("{}", usage_str());
                return 0;
            }
            other => {
                eprintln!("mac-clamshell-mcp watch: unknown argument \"{other}\"");
                return 2;
            }
        }
        i += 1;
    }

    let data_dir = resolve_data_dir(data_dir_opt.as_deref());
    let res = if daemon {
        run_watch_daemon(&data_dir, interval_secs)
    } else {
        run_watch_once(&data_dir)
    };
    if let Err(e) = res {
        eprintln!("mac-clamshell-mcp watch: {e}");
        return 1;
    }
    0
}
