use crate::i18n::Lang;
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

impl CpuTopology {
    pub fn is_heterogeneous(&self) -> bool {
        self.performance_cores > 0
    }
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
    let is_hetero = topo.is_heterogeneous();

    match profile {
        "strict" => ("strict".to_string(), 1, "-b".to_string(), 19),
        "eco" => {
            let mut max_jobs = if is_hetero {
                topo.efficiency_cores.max(1).min(2)
            } else if total >= 4 {
                2
            } else {
                1
            };
            if respect_low_batt && battery_pct.map(|p| p <= 20).unwrap_or(false) {
                max_jobs = 1;
            }
            ("eco".to_string(), max_jobs, "-c background".to_string(), 15)
        }
        "utility" => {
            let max_jobs = if is_hetero {
                topo.efficiency_cores.max(1).clamp(2, 4)
            } else {
                (total / 2).max(2).min(total)
            };
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
    let thermal_throttled = matches!(thermal_state, "warn" | "serious" | "critical");
    let auto_profile = if thermal_throttled {
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
        "NINJAJOBS",
        "OMP_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
        "MKL_NUM_THREADS",
        "VECLIB_MAXIMUM_THREADS",
        "RAYON_NUM_THREADS",
        "UV_THREADPOOL_SIZE",
        "GOMAXPROCS",
        "PYTEST_XDIST_AUTO_NUM_WORKERS",
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

pub fn format_lid(lid_state: &str, lang: Lang) -> &'static str {
    match (lang, lid_state) {
        (Lang::ZhCn, "closed") => "合盖",
        (Lang::ZhCn, "open") => "开盖",
        (Lang::ZhCn, _) => "未知",
        (Lang::En, "closed") => "closed",
        (Lang::En, "open") => "open",
        (Lang::En, _) => "unknown",
    }
}

pub fn format_power(power_source: &str, battery_pct: Option<u32>, lang: Lang) -> String {
    match (lang, power_source, battery_pct) {
        (Lang::ZhCn, "battery", Some(p)) => format!("电池 {p}%"),
        (Lang::ZhCn, "battery", None) => "电池供电".to_string(),
        (Lang::ZhCn, "ac", Some(p)) => format!("外接电源 {p}%"),
        (Lang::ZhCn, "ac", None) => "外接电源".to_string(),
        (Lang::ZhCn, _, _) => "未知电源".to_string(),
        (Lang::En, "battery", Some(p)) => format!("Battery {p}%"),
        (Lang::En, "battery", None) => "Battery".to_string(),
        (Lang::En, "ac", Some(p)) => format!("AC Power {p}%"),
        (Lang::En, "ac", None) => "AC Power".to_string(),
        (Lang::En, _, _) => "Unknown power".to_string(),
    }
}

pub fn format_topology(topo: &CpuTopology, lang: Lang) -> String {
    if topo.is_heterogeneous() {
        match lang {
            Lang::ZhCn => format!(
                "{} 核 ({} 性能核 + {} 能效核)",
                topo.total, topo.performance_cores, topo.efficiency_cores
            ),
            Lang::En => format!(
                "{} cores ({} P-cores + {} E-cores)",
                topo.total, topo.performance_cores, topo.efficiency_cores
            ),
        }
    } else {
        match lang {
            Lang::ZhCn => format!("{} 核", topo.total),
            Lang::En => format!("{} cores", topo.total),
        }
    }
}

pub fn format_topology_compact(topo: &CpuTopology) -> String {
    if topo.is_heterogeneous() {
        format!("{}P+{}E", topo.performance_cores, topo.efficiency_cores)
    } else {
        format!("{} cores", topo.total)
    }
}

pub fn format_thermal(
    thermal_state: &str,
    thermal_pressure: &str,
    cpu_speed_limit_pct: Option<u32>,
    lang: Lang,
) -> String {
    let p_str = match (lang, thermal_pressure) {
        (Lang::ZhCn, "nominal") => "正常 (nominal)",
        (Lang::ZhCn, "fair") => "轻度温升 (fair)",
        (Lang::ZhCn, "serious") => "偏高降频 (serious)",
        (Lang::ZhCn, "critical") => "严重过热 (critical)",
        (Lang::ZhCn, _) => {
            if thermal_state == "warn" {
                "温控告警"
            } else {
                "温度正常"
            }
        }
        (Lang::En, "nominal") => "nominal",
        (Lang::En, "fair") => "fair",
        (Lang::En, "serious") => "serious (throttling)",
        (Lang::En, "critical") => "critical (throttling)",
        (Lang::En, _) => thermal_state,
    };
    match cpu_speed_limit_pct {
        Some(pct) => format!("{p_str}, CPU_Speed_Limit={pct}%"),
        None => p_str.to_string(),
    }
}

pub fn format_profile(policy: &Policy, topo: &CpuTopology, lang: Lang) -> String {
    let hetero = topo.is_heterogeneous();
    match (lang, policy.profile.as_str(), hetero) {
        (Lang::ZhCn, "strict", true) => {
            "强限保护模式（E-Core + I/O 限流，单并发）".to_string()
        }
        (Lang::ZhCn, "strict", false) => {
            "强限保护模式（Background QoS + nice + I/O 限流，单并发）".to_string()
        }
        (Lang::ZhCn, "eco", true) => {
            format!("合盖节能模式（严格 E-Core，最高 {} 并发）", policy.max_jobs)
        }
        (Lang::ZhCn, "eco", false) => {
            format!(
                "合盖节能模式（Background QoS + nice，最高 {} 并发）",
                policy.max_jobs
            )
        }
        (Lang::ZhCn, "utility", true) => {
            format!("合盖控温模式（Utility QoS，最高 {} 并发）", policy.max_jobs)
        }
        (Lang::ZhCn, "utility", false) => {
            format!(
                "合盖控温模式（Utility QoS + nice，最高 {} 并发）",
                policy.max_jobs
            )
        }
        (Lang::ZhCn, _, _) => format!("正常性能模式（{} 并发）", policy.max_jobs),
        (Lang::En, "strict", true) => {
            "Strict thermal protection (E-Core + I/O throttle, 1 job)".to_string()
        }
        (Lang::En, "strict", false) => {
            "Strict thermal protection (Background QoS + nice + I/O throttle, 1 job)".to_string()
        }
        (Lang::En, "eco", true) => {
            format!("Eco clamshell mode (E-Core pinned, up to {} jobs)", policy.max_jobs)
        }
        (Lang::En, "eco", false) => {
            format!(
                "Eco clamshell mode (Background QoS + nice, up to {} jobs)",
                policy.max_jobs
            )
        }
        (Lang::En, "utility", true) => {
            format!(
                "Utility clamshell mode (Utility QoS, up to {} jobs)",
                policy.max_jobs
            )
        }
        (Lang::En, "utility", false) => {
            format!(
                "Utility clamshell mode (Utility QoS + nice, up to {} jobs)",
                policy.max_jobs
            )
        }
        (Lang::En, _, _) => format!("Normal performance (up to {} jobs)", policy.max_jobs),
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
    fn policy_matrix_covers_apple_silicon_intel_and_thermal_pressure() {
        let topo = m1_pro();
        let def = Settings::default();

        // Closed + battery 90% -> eco, 2 jobs
        let p = compute_policy("closed", "battery", Some(90), "ok", &topo, &def);
        assert_eq!(p.profile, "eco");
        assert_eq!(p.max_jobs, 2);
        assert_eq!(p.taskpolicy, "-c background");
        assert_eq!(p.nice_level, 15);
        let envs = build_env_vars(&p);
        assert_eq!(envs.get("NINJAJOBS").map(String::as_str), Some("2"));
        assert_eq!(
            envs.get("PYTEST_XDIST_AUTO_NUM_WORKERS").map(String::as_str),
            Some("2")
        );

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

        // Intel Mac homogeneous 16 cores -> utility = max(2, 16 / 2) = 8, says Background QoS + nice on eco
        let intel = CpuTopology {
            total: 16,
            performance_cores: 0,
            efficiency_cores: 0,
        };
        let p_intel_u = compute_policy("closed", "ac", Some(100), "ok", &intel, &def);
        assert_eq!(p_intel_u.profile, "utility");
        assert_eq!(p_intel_u.max_jobs, 8);
        assert_eq!(format_topology(&intel, Lang::En), "16 cores");

        let p_intel_eco = compute_policy("closed", "battery", Some(85), "ok", &intel, &def);
        let desc_en = format_profile(&p_intel_eco, &intel, Lang::En);
        assert!(desc_en.contains("Background QoS + nice"));
        assert!(!desc_en.contains("E-Core"));

        // Serious / critical thermal pressure -> strict regardless of lid open
        for therm in ["warn", "serious", "critical"] {
            let p = compute_policy("open", "ac", Some(100), therm, &topo, &def);
            assert_eq!(p.profile, "strict");
            assert_eq!(p.max_jobs, 1);
            assert_eq!(p.taskpolicy, "-b");
            assert_eq!(p.nice_level, 19);
        }

        // Manual override + clamped maxJobsOverride
        let manual = Settings {
            mode_override: "normal".into(),
            max_jobs_override: 64,
            ac_closed_profile: "eco".into(),
            ..Settings::default()
        };
        let p = compute_policy("closed", "battery", Some(10), "warn", &topo, &manual);
        assert_eq!(p.profile, "normal");
        assert_eq!(p.auto_profile, "strict");
        assert_eq!(p.mode_source, "manual");
        assert_eq!(p.max_jobs, 8);
        assert!(build_env_vars(&p).is_empty());
    }
}
