use crate::guard::{read_display_info, run_watch_daemon, run_watch_once, set_brightness_synced};
use crate::hook::run_hook;
use crate::mcp::server::serve_stdio;
use crate::mcp::telemetry::TelemetryEngine;
use crate::platform::display_ffi::MacOsDisplayBackend;
use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use crate::policy::{format_lid_zh, format_power_zh, format_profile_zh};
use crate::runner::{execute_run, RunOptions};
use crate::state::{load_guard_state, resolve_data_dir};
use serde_json::json;
use std::path::PathBuf;

const USAGE: &str = "\
Mac Clamshell MCP — native macOS clamshell throttling, display guard, brightness & MCP server

USAGE:
  mac-clamshell-mcp <COMMAND> [OPTIONS]

COMMANDS:
  serve [--data-dir DIR] [--panel-html PATH]
      Run the MCP stdio JSON-RPC server (tools + SEP-1865 dashboard resource).

  status [--json] [--data-dir DIR]
      Inspect live macOS lid state, power, thermal, display brightness, and active policy.

  brightness [get | set <0..100>] [--json] [--lid-state open|closed] [--data-dir DIR]
      Read or set built-in display brightness (0-100%) in sync with display guard wake-restore.

  run [--mode auto|normal|utility|eco|strict] [--jobs N] [--quiet|-q] [--data-dir DIR]
      (--sh \"<cmd>\" | [--] <cmd> [args...])
      Execute a command wrapped with live taskpolicy, nice, and concurrency env limits.

  hook <session-start | pre-invocation> [--data-dir DIR]
      Antigravity lifecycle hook (early-exit JSON stdin -> JSON stdout, always fail-open).

  watch [--daemon] [--interval SECS] [--data-dir DIR]
      Run the display thermal guard once (for LaunchAgent StartInterval) or as a daemon.

  --version, -V    Print version
  --help, -h       Print help";

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
        println!("{USAGE}");
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
        "run" => cmd_run(&args[1..]),
        "hook" => cmd_hook(&args[1..]),
        "watch" => cmd_watch(&args[1..]),
        other => {
            eprintln!("mac-clamshell-mcp: unknown command \"{other}\"\n\n{USAGE}");
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
                println!("{USAGE}");
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
                println!("{USAGE}");
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

    let disp_str = if s.display.available {
        if s.lid_state == "closed" {
            format!(
                "asleep (restore target {}%)",
                s.display.saved_percent.unwrap_or(0)
            )
        } else if s.display.asleep {
            format!(
                "asleep (target {}%)",
                s.display.saved_percent.or(s.display.percent).unwrap_or(0)
            )
        } else {
            format!("{}% (awake)", s.display.percent.unwrap_or(0))
        }
    } else {
        "unavailable".to_string()
    };

    println!("Mac Clamshell Status");
    println!("  Lid State      : {} ({})", s.lid_state, format_lid_zh(&s.lid_state));
    println!(
        "  Power Source   : {} ({})",
        s.power_source,
        format_power_zh(&s.power_source, s.battery_pct)
    );
    println!(
        "  Thermal State  : {} (CPU_Speed_Limit={}%)",
        s.thermal_state, s.cpu_speed_limit_pct
    );
    println!("  Low Power Mode : {}", s.low_power_mode);
    println!("  Display        : {disp_str}");
    println!(
        "  CPU Topology   : {} total ({} P-cores + {} E-cores)",
        s.cpu_topology.total, s.cpu_topology.performance_cores, s.cpu_topology.efficiency_cores
    );
    println!(
        "  Active Profile : {} [{}] — {}",
        s.policy.profile,
        s.policy.mode_source,
        format_profile_zh(&s.policy)
    );
    let tp = if s.policy.taskpolicy.is_empty() {
        "none"
    } else {
        s.policy.taskpolicy.as_str()
    };
    println!(
        "  Throttle Cmd   : {tp} (nice={}, maxJobs={})",
        s.policy.nice_level, s.policy.max_jobs
    );
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
                        eprintln!("mac-clamshell-mcp brightness: --lid-state must be open or closed");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("mac-clamshell-mcp brightness: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
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
            println!("{}", serde_json::to_string(&info).unwrap_or_else(|_| "{}".into()));
        } else if info.available {
            match info.percent {
                Some(p) => println!("{p}% (asleep={}, saved={}%, mode={})", info.asleep, info.saved_percent.unwrap_or(p), info.mode),
                None => println!("asleep (saved={}%)", info.saved_percent.unwrap_or(20)),
            }
        } else {
            println!("unavailable");
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
            if json_mode {
                println!("{}", serde_json::to_string(&action).unwrap_or_else(|_| "{}".into()));
            } else if action.mode == "saved_target" {
                println!("saved restore target: {}% (deferred={})", action.saved_percent, action.deferred);
            } else {
                println!("brightness set to {}%", action.percent.unwrap_or(action.requested_percent));
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
                println!("{USAGE}");
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
    println!("{}", serde_json::to_string(&output).unwrap_or_else(|_| "{}".into()));
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
                println!("{USAGE}");
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
