use crate::guard::{read_display_info, run_watch_daemon, run_watch_once, set_brightness_synced};
use crate::hook::run_hook;
use crate::i18n::{detect_lang, parse_lang_tag, Lang};
use crate::launchagent::{
    guard_service_status, install_launchagent, uninstall_launchagent, DEFAULT_GUARD_INTERVAL_SECS,
};
use crate::mcp::server::serve_stdio;
use crate::mcp::telemetry::TelemetryEngine;
use crate::platform::display_ffi::MacOsDisplayBackend;
use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use crate::policy::{format_lid, format_power, format_profile, format_thermal, format_topology};
use crate::runner::{execute_run, RunOptions};
use crate::state::{load_guard_state, read_settings, resolve_data_dir};
use serde_json::json;
use std::path::PathBuf;

const USAGE_EN: &str = "\
Mac Clamshell MCP — native macOS clamshell throttling, display guard, brightness & MCP server

USAGE:
  mac-clamshell-mcp <COMMAND> [OPTIONS]

COMMANDS:
  serve [--data-dir DIR] [--panel-html PATH]
      Run the MCP stdio JSON-RPC server (active display guard + SEP-1865 dashboard).

  status [--json] [--data-dir DIR]
      Inspect live macOS lid state, power, thermal pressure, display brightness, and active policy.

  brightness [get | set <0..100>] [--json] [--lid-state open|closed] [--data-dir DIR]
      Read or set built-in display brightness (0-100%) in sync with display guard wake-restore.

  guard <install | uninstall | status> [--interval SECS] [--bin PATH] [--json] [--data-dir DIR]
      Install, remove, or inspect the standalone 24/7 macOS LaunchAgent display guard.

  run [--mode auto|normal|utility|eco|strict] [--jobs N] [--quiet|-q] [--data-dir DIR]
      (--sh \"<cmd>\" | [--] <cmd> [args...])
      Execute a command wrapped with live taskpolicy, nice, and concurrency env limits.

  hook <session-start | pre-invocation> [--data-dir DIR]
      Antigravity lifecycle hook (early-exit JSON stdin -> JSON stdout, always fail-open).

  watch [--daemon] [--interval SECS] [--data-dir DIR]
      Run one display guard check (for LaunchAgent StartInterval) or loop as a foreground daemon.

  --version, -V    Print version
  --help, -h       Print help";

const USAGE_ZH: &str = "\
Mac Clamshell MCP — macOS 合盖性能调控、熄屏护眼守护、内建亮度与 MCP 服务

用法:
  mac-clamshell-mcp <命令> [选项]

命令:
  serve [--data-dir DIR] [--panel-html PATH]
      启动 MCP stdio JSON-RPC 服务（内置合盖熄屏守护 + SEP-1865 控制面板）。

  status [--json] [--data-dir DIR]
      查看当前开合盖、供电、热压力、内建屏幕亮度、守护状态与生效策略。

  brightness [get | set <0..100>] [--json] [--lid-state open|closed] [--data-dir DIR]
      读取或设置内建屏幕亮度（0-100%），合盖时安全更新开盖恢复目标。

  guard <install | uninstall | status> [--interval SECS] [--bin PATH] [--json] [--data-dir DIR]
      安装、卸载或查看独立常驻的 macOS LaunchAgent 合盖熄屏守护。

  run [--mode auto|normal|utility|eco|strict] [--jobs N] [--quiet|-q] [--data-dir DIR]
      (--sh \"<cmd>\" | [--] <cmd> [args...])
      按当前状态使用 taskpolicy、nice 与构建并发环境变量包裹执行命令。

  hook <session-start | pre-invocation> [--data-dir DIR]
      Antigravity 生命周期钩子（快速平衡 JSON 读取，失败默认放行）。

  watch [--daemon] [--interval SECS] [--data-dir DIR]
      单次执行或前台常驻运行合盖熄屏与热限亮守护。

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
        MacOsProbeBackend,
        MacOsDisplayBackend,
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
                if s.lid_state == "closed" || s.display.mode == "saved_target" {
                    let ext = if s.display.external_count > 0 {
                        format!("（外接 {} 台）", s.display.external_count)
                    } else {
                        String::new()
                    };
                    format!("已合盖，恢复目标 {}%{ext}", s.display.saved_percent.unwrap_or(20))
                } else if s.display.asleep {
                    format!(
                        "已熄屏，恢复目标 {}%",
                        s.display.saved_percent.or(s.display.percent).unwrap_or(20)
                    )
                } else {
                    format!("{}%（点亮）", s.display.percent.unwrap_or(0))
                }
            } else {
                "未检测到内建屏幕".to_string()
            };
            let lpm_zh = match s.low_power_mode.as_str() {
                "1" => "已开启",
                "0" => "未开启",
                _ => "未知",
            };
            let guard_zh = match s.guard_status.as_str() {
                "protecting" => "熄屏保护中",
                "thermal_cap" => "温控限亮中",
                "armed" => "常驻就绪",
                "paused" => "已暂停",
                _ => "空闲（MCP 运行时自动激活）",
            };
            let src_zh = if s.policy.mode_source == "manual" {
                "手动锁定"
            } else {
                "自动识别"
            };

            println!("Mac Clamshell 状态");
            println!("  盒盖状态   : {}", format_lid(&s.lid_state, lang));
            println!(
                "  电源供电   : {}",
                format_power(&s.power_source, s.battery_pct, lang)
            );
            println!("  温控压力   : {therm_str}");
            println!("  低电量模式 : {lpm_zh}");
            println!("  内建屏幕   : {disp_str}");
            println!("  熄屏守护   : {guard_zh}");
            println!("  处理器拓扑 : {topo_str}");
            println!("  生效档位   : {} [{src_zh}] — {prof_str}", s.policy.profile);
            println!(
                "  调度参数   : {tp} (nice={}, 并发上限={})",
                s.policy.nice_level, s.policy.max_jobs
            );
        }
        Lang::En => {
            let disp_str = if s.display.available {
                if s.lid_state == "closed" || s.display.mode == "saved_target" {
                    let ext = if s.display.external_count > 0 {
                        format!(" ({} external)", s.display.external_count)
                    } else {
                        String::new()
                    };
                    format!(
                        "closed, saved target {}%{ext}",
                        s.display.saved_percent.unwrap_or(20)
                    )
                } else if s.display.asleep {
                    format!(
                        "asleep, saved target {}%",
                        s.display.saved_percent.or(s.display.percent).unwrap_or(20)
                    )
                } else {
                    format!("{}% (awake)", s.display.percent.unwrap_or(0))
                }
            } else {
                "unavailable".to_string()
            };
            let lpm_en = match s.low_power_mode.as_str() {
                "1" => "on",
                "0" => "off",
                _ => "unknown",
            };

            println!("Mac Clamshell Status");
            println!("  Lid State      : {}", format_lid(&s.lid_state, lang));
            println!(
                "  Power Source   : {}",
                format_power(&s.power_source, s.battery_pct, lang)
            );
            println!("  Thermal        : {therm_str}");
            println!("  Low Power Mode : {lpm_en}");
            println!("  Display        : {disp_str}");
            println!("  Display Guard  : {}", s.guard_status);
            println!("  CPU Topology   : {topo_str}");
            println!(
                "  Active Profile : {} [{}] — {prof_str}",
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
    let lang = detect_lang(Some(&settings));
    let probe = MacOsProbeBackend;
    let display = MacOsDisplayBackend;

    if subcmd == "get" {
        let lid_str = match lid_override.or_else(|| probe.lid_closed_fast()) {
            Some(true) => "closed",
            Some(false) => "open",
            None => "unknown",
        };
        let state = load_guard_state(&data_dir);
        let info = read_display_info(&display, lid_str, &state);
        if json_mode {
            println!(
                "{}",
                serde_json::to_string(&info).unwrap_or_else(|_| "{}".into())
            );
        } else if info.available {
            if lid_str == "closed" || info.mode == "saved_target" {
                let sv = info.saved_percent.unwrap_or(20);
                match lang {
                    Lang::ZhCn => println!("已合盖，恢复目标 {sv}%"),
                    Lang::En => println!("closed, saved target {sv}%"),
                }
            } else if info.asleep {
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
                    Lang::ZhCn => println!(
                        "已保存开盖恢复亮度目标: {}% (延迟应用={})",
                        action.saved_percent, action.deferred
                    ),
                    Lang::En => println!(
                        "saved restore target: {}% (deferred={})",
                        action.saved_percent, action.deferred
                    ),
                }
            } else {
                let p = action.percent.unwrap_or(action.requested_percent);
                match lang {
                    Lang::ZhCn => println!("内建屏幕亮度已设为 {p}%"),
                    Lang::En => println!("brightness set to {p}%"),
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
            s if s.starts_with("--interval=") => {
                match s["--interval=".len()..].parse::<u64>() {
                    Ok(n) if n >= 2 => interval_secs = n,
                    _ => {
                        eprintln!("mac-clamshell-mcp guard: --interval must be >= 2 seconds");
                        return 2;
                    }
                }
            }
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
    let lang = detect_lang(Some(&settings));

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
                        println!("Mac Clamshell 熄屏守护状态");
                        println!(
                            "  守护开关     : {}",
                            if st.guard_enabled { "已启用" } else { "已暂停" }
                        );
                        println!(
                            "  LaunchAgent  : {} ({})",
                            if st.loaded {
                                "运行中"
                            } else if st.plist_installed {
                                "已安装未加载"
                            } else {
                                "未安装（仅 Antigravity 打开时生效）"
                            },
                            st.plist_path
                        );
                        if let Some(iv) = st.interval_secs {
                            println!("  检查间隔     : {iv} 秒");
                        }
                    }
                    Lang::En => {
                        println!("Mac Clamshell Display Guard");
                        println!(
                            "  Guard Toggle : {}",
                            if st.guard_enabled { "enabled" } else { "paused" }
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
                    if v.is_empty() || v.starts_with('0') || !v.bytes().all(|b| b.is_ascii_digit()) {
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
