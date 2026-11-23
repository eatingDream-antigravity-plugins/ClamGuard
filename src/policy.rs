use crate::state::Settings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuTopology {
    pub total: u32,
    pub performance_cores: u32,
    pub efficiency_cores: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub profile: String,
    pub auto_profile: String,
    pub mode_source: String,
    pub max_jobs: u32,
    pub taskpolicy: String,
    pub nice_level: i32,
}

/// Apply canonical profile defaults (max_jobs, taskpolicy, nice_level).
pub fn apply_profile_defaults(
    profile: &str,
    topo: &CpuTopology,
    battery_pct: Option<u32>,
    respect_low_batt: bool,
) -> (String, u32, String, i32) {
    let total = topo.total.max(1);
    let e_cores = topo.efficiency_cores.max(1);

    match profile {
        "strict" => ("strict".to_string(), 1, "-b".to_string(), 19),
        "eco" => {
            let mut max_jobs = e_cores.min(2);
            if respect_low_batt && battery_pct.map(|p| p <= 20).unwrap_or(false) {
                max_jobs = 1;
            }
            ("eco".to_string(), max_jobs, "-c background".to_string(), 15)
        }
        "utility" => {
            let max_jobs = e_cores.clamp(2, 4);
            ("utility".to_string(), max_jobs, "-c utility".to_string(), 10)
        }
        _ => ("normal".to_string(), total, String::new(), 0),
    }
}

pub fn compute_policy(
    lid_state: &str,
    power_source: &str,
    battery_pct: Option<u32>,
    thermal_state: &str,
    topo: &CpuTopology,
    settings: &Settings,
) -> Policy {
    let auto_profile = if thermal_state == "warn" {
        "strict".to_string()
    } else if lid_state == "closed" && power_source == "battery" {
        "eco".to_string()
    } else if lid_state == "closed" {
        if settings.ac_closed_profile == "eco" {
            "eco".to_string()
        } else {
            "utility".to_string()
        }
    } else {
        "normal".to_string()
    };

    let (requested, mode_source, respect_low_batt) =
        if !settings.mode_override.is_empty() && settings.mode_override != "auto" {
            (settings.mode_override.as_str(), "manual", false)
        } else {
            (auto_profile.as_str(), "auto", true)
        };

    let (profile, mut max_jobs, taskpolicy, nice_level) =
        apply_profile_defaults(requested, topo, battery_pct, respect_low_batt);

    if settings.max_jobs_override > 0 {
        max_jobs = settings.max_jobs_override.min(topo.total.max(1));
    }

    Policy {
        profile,
        auto_profile,
        mode_source: mode_source.to_string(),
        max_jobs,
        taskpolicy,
        nice_level,
    }
}

pub fn build_env_vars(policy: &Policy) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if policy.profile == "normal" {
        return map;
    }
    let j = policy.max_jobs.to_string();
    map.insert("MAKEFLAGS".to_string(), format!("-j{j}"));
    for key in [
        "CARGO_BUILD_JOBS",
        "CMAKE_BUILD_PARALLEL_LEVEL",
        "OMP_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
        "MKL_NUM_THREADS",
        "VECLIB_MAXIMUM_THREADS",
        "RAYON_NUM_THREADS",
        "UV_THREADPOOL_SIZE",
        "GOMAXPROCS",
    ] {
        map.insert(key.to_string(), j.clone());
    }
    map
}

pub fn state_signature(
    lid_state: &str,
    power_source: &str,
    thermal_state: &str,
    policy: &Policy,
) -> String {
    format!(
        "{lid_state}|{power_source}|{thermal_state}|{}|{}|{}",
        policy.profile, policy.max_jobs, policy.mode_source
    )
}

pub fn format_lid_zh(lid_state: &str) -> &'static str {
    match lid_state {
        "closed" => "合盖",
        "open" => "开盖",
        _ => "未知",
    }
}

pub fn format_power_zh(power_source: &str, battery_pct: Option<u32>) -> String {
    match power_source {
        "battery" => match battery_pct {
            Some(p) => format!("电池 {p}%"),
            None => "电池供电".to_string(),
        },
        "ac" => match battery_pct {
            Some(p) => format!("外接电源 {p}%"),
            None => "外接电源".to_string(),
        },
        _ => "未知电源".to_string(),
    }
}

pub fn format_profile_zh(policy: &Policy) -> String {
    match policy.profile.as_str() {
        "strict" => "Strict 过热保护模式（E-Core + I/O 限流，并发=1）".to_string(),
        "eco" => format!("Eco 合盖节能模式（严格 E-Core，并发≤{}）", policy.max_jobs),
        "utility" => format!("Utility 合盖控温模式（Utility QoS，并发≤{}）", policy.max_jobs),
        _ => format!("Normal 正常性能模式（并发≤{}）", policy.max_jobs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m1_pro() -> CpuTopology {
        CpuTopology {
            total: 8,
            performance_cores: 6,
            efficiency_cores: 2,
        }
    }

    #[test]
    fn policy_matrix_covers_all_tiers() {
        let topo = m1_pro();
        let def = Settings::default();

        // Closed + battery 90% -> eco, 2 jobs
        let p = compute_policy("closed", "battery", Some(90), "ok", &topo, &def);
        assert_eq!(p.profile, "eco");
        assert_eq!(p.max_jobs, 2);
        assert_eq!(p.taskpolicy, "-c background");
        assert_eq!(p.nice_level, 15);

        // Closed + low battery 15% -> eco, 1 job
        let p = compute_policy("closed", "battery", Some(15), "ok", &topo, &def);
        assert_eq!(p.profile, "eco");
        assert_eq!(p.max_jobs, 1);

        // Closed + AC -> utility, 2..4 jobs
        let p = compute_policy("closed", "ac", Some(100), "ok", &topo, &def);
        assert_eq!(p.profile, "utility");
        assert_eq!(p.max_jobs, 2);
        assert_eq!(p.taskpolicy, "-c utility");

        // Large Apple Silicon (6 E-cores) clamps utility to 4
        let big = CpuTopology {
            total: 16,
            performance_cores: 10,
            efficiency_cores: 6,
        };
        let p = compute_policy("closed", "ac", Some(100), "ok", &big, &def);
        assert_eq!(p.max_jobs, 4);

        // Thermal warn -> strict regardless of lid open
        let p = compute_policy("open", "ac", Some(100), "warn", &topo, &def);
        assert_eq!(p.profile, "strict");
        assert_eq!(p.max_jobs, 1);
        assert_eq!(p.taskpolicy, "-b");
        assert_eq!(p.nice_level, 19);

        // Manual override + clamped maxJobsOverride
        let manual = Settings {
            mode_override: "normal".into(),
            max_jobs_override: 64,
            ac_closed_profile: "eco".into(),
        };
        let p = compute_policy("closed", "battery", Some(10), "warn", &topo, &manual);
        assert_eq!(p.profile, "normal");
        assert_eq!(p.auto_profile, "strict");
        assert_eq!(p.mode_source, "manual");
        assert_eq!(p.max_jobs, 8);
        assert!(build_env_vars(&p).is_empty());
    }
}
