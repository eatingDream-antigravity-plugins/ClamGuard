use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use crate::policy::{apply_profile_defaults, build_env_vars, compute_policy};
use crate::state::read_settings;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub mode: String,
    pub override_jobs: Option<u32>,
    pub quiet: bool,
    pub sh_cmd: Option<String>,
    pub cmd_args: Vec<String>,
}

fn apply_concurrency_env(cmd: &mut Command, max_jobs: u32) {
    let flag = format!("-j{max_jobs}");
    let existing = std::env::var("MAKEFLAGS").unwrap_or_default();
    let padded = format!(" {existing} ");
    let next_makeflags = if padded.contains(&format!(" {flag} ")) {
        existing
    } else if existing.trim().is_empty() {
        flag
    } else {
        format!("{existing} {flag}")
    };
    cmd.env("MAKEFLAGS", next_makeflags);

    let dummy = crate::policy::Policy {
        profile: "eco".into(),
        auto_profile: "eco".into(),
        mode_source: "auto".into(),
        max_jobs,
        taskpolicy: String::new(),
        nice_level: 0,
    };
    for (k, v) in build_env_vars(&dummy) {
        if k != "MAKEFLAGS" {
            cmd.env(k, v);
        }
    }
}

pub fn execute_run(data_dir: &Path, opts: RunOptions) -> Result<i32, String> {
    if opts.sh_cmd.is_none() && opts.cmd_args.is_empty() {
        return Err("missing command to execute".to_string());
    }

    let probe = MacOsProbeBackend;
    let host = probe.static_host();
    let hw = probe.probe_hardware();
    let settings = read_settings(data_dir);
    let mut policy = compute_policy(
        &hw.lid_state,
        &hw.power_source,
        hw.battery_pct,
        &hw.thermal_state,
        &host.cpu_topology,
        &settings,
    );

    match opts.mode.as_str() {
        "auto" => {}
        "normal" | "utility" | "eco" | "strict" => {
            let (prof, jobs, tp, nice) =
                apply_profile_defaults(&opts.mode, &host.cpu_topology, hw.battery_pct, false);
            policy.profile = prof;
            policy.max_jobs = jobs;
            policy.taskpolicy = tp;
            policy.nice_level = nice;
        }
        other => return Err(format!("unknown mode \"{other}\"")),
    }

    if let Some(j) = opts.override_jobs {
        if j == 0 {
            return Err("--jobs must be a positive integer".to_string());
        }
        policy.max_jobs = j;
    }

    let target_argv: Vec<String> = if let Some(sh) = opts.sh_cmd {
        vec!["/bin/zsh".to_string(), "-c".to_string(), sh]
    } else {
        opts.cmd_args
    };

    if policy.profile == "normal" {
        let mut cmd = Command::new(&target_argv[0]);
        cmd.args(&target_argv[1..]);
        cmd.env("MAC_CLAMSHELL_PROFILE", &policy.profile);
        cmd.env("MAC_CLAMSHELL_MAX_JOBS", policy.max_jobs.to_string());
        if opts.override_jobs.is_some() {
            apply_concurrency_env(&mut cmd, policy.max_jobs);
        }
        let err = cmd.exec();
        return Err(format!("exec {} failed: {err}", target_argv[0]));
    }

    if !opts.quiet {
        eprintln!(
            "[mac-clamshell] profile={} lid={} power={} jobs={} ({} nice={})",
            policy.profile,
            hw.lid_state,
            hw.power_source,
            policy.max_jobs,
            policy.taskpolicy,
            policy.nice_level
        );
    }

    let nice_str = policy.nice_level.to_string();
    let mut cmd = match policy.taskpolicy.as_str() {
        "-b" => {
            let mut c = Command::new("/usr/sbin/taskpolicy");
            c.args(["-b", "/usr/bin/nice", "-n", &nice_str, "--"]);
            c
        }
        "-c background" => {
            let mut c = Command::new("/usr/sbin/taskpolicy");
            c.args(["-c", "background", "/usr/bin/nice", "-n", &nice_str, "--"]);
            c
        }
        "-c utility" => {
            let mut c = Command::new("/usr/sbin/taskpolicy");
            c.args(["-c", "utility", "/usr/bin/nice", "-n", &nice_str, "--"]);
            c
        }
        _ => {
            let mut c = Command::new("/usr/bin/nice");
            c.args(["-n", &nice_str, "--"]);
            c
        }
    };

    cmd.args(&target_argv);
    cmd.env("MAC_CLAMSHELL_PROFILE", &policy.profile);
    cmd.env("MAC_CLAMSHELL_MAX_JOBS", policy.max_jobs.to_string());
    apply_concurrency_env(&mut cmd, policy.max_jobs);

    let err = cmd.exec();
    Err(format!("exec failed: {err}"))
}
