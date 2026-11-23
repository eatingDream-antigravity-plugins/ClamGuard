use crate::guard::{
    read_display_info, set_brightness_synced, BrightnessAction, DisplayGuardSummary, DisplayInfo,
};
use crate::platform::display_ffi::DisplayBackend;
use crate::platform::macos::{MemorySummary, ProbeBackend};
use crate::policy::{build_env_vars, compute_policy, CpuTopology, Policy};
use crate::state::{
    abbreviate_home, format_iso8601_utc, load_guard_state, load_guard_state_opt, now_epoch_ms,
    read_settings, settings_path, sha256_hex_16, write_settings, Settings, SettingsPatch,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

pub const TELEMETRY_CACHE_TTL_MS: u64 = 2500;

fn lock_unpoisoned<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostTelemetry {
    pub hostname: String,
    pub chip: String,
    pub macos_version: String,
    pub uptime_seconds: u64,
    pub load_avg: [f64; 3],
    pub memory: MemorySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryPaths {
    pub settings_path: String,
    pub binary_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySnapshot {
    pub timestamp: String,
    pub host: HostTelemetry,
    pub lid_state: String,
    pub clamshell_causes_sleep: String,
    pub power_source: String,
    pub battery_pct: Option<u32>,
    pub battery_detail: String,
    pub thermal_state: String,
    pub cpu_speed_limit_pct: u32,
    pub low_power_mode: String,
    pub cpu_topology: CpuTopology,
    pub settings: Settings,
    pub policy: Policy,
    pub env_vars: BTreeMap<String, String>,
    pub display: DisplayInfo,
    pub display_guard: Option<DisplayGuardSummary>,
    pub degraded: bool,
    pub paths: TelemetryPaths,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct CollectedTelemetry {
    pub snapshot: Arc<TelemetrySnapshot>,
    pub server_ms: u64,
    pub from_cache: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnchangedPayload {
    pub unchanged: bool,
    pub hash: String,
    pub timestamp: String,
    pub server_ms: u64,
    pub from_cache: bool,
}

pub fn compute_telemetry_hash(s: &TelemetrySnapshot) -> String {
    let canonical = json!({
        "host": {
            "hostname": s.host.hostname,
            "chip": s.host.chip,
            "macosVersion": s.host.macos_version,
            "uptimeMinutes": s.host.uptime_seconds / 60,
            "loadAvg": s.host.load_avg,
            "memory": s.host.memory,
        },
        "lidState": s.lid_state,
        "clamshellCausesSleep": s.clamshell_causes_sleep,
        "powerSource": s.power_source,
        "batteryPct": s.battery_pct,
        "batteryDetail": s.battery_detail,
        "thermalState": s.thermal_state,
        "cpuSpeedLimitPct": s.cpu_speed_limit_pct,
        "lowPowerMode": s.low_power_mode,
        "cpuTopology": s.cpu_topology,
        "settings": s.settings,
        "policy": s.policy,
        "envVars": s.env_vars,
        "display": s.display,
        "displayGuard": s.display_guard,
        "degraded": s.degraded,
    });
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    sha256_hex_16(&bytes)
}

pub fn summarize_for_model(s: &TelemetrySnapshot) -> String {
    let lid_label = match s.lid_state.as_str() {
        "closed" => "closed (合盖)",
        "open" => "open (开盖)",
        _ => "unknown",
    };
    let power_label = match (s.power_source.as_str(), s.battery_pct) {
        ("battery", Some(p)) => format!("battery {p}%"),
        ("battery", None) => "battery".to_string(),
        ("ac", Some(p)) => format!("AC {p}%"),
        ("ac", None) => "AC".to_string(),
        _ => "unknown".to_string(),
    };
    let tp = if s.policy.taskpolicy.is_empty() {
        "none"
    } else {
        s.policy.taskpolicy.as_str()
    };
    let mut lines = vec![
        format!("Mac Clamshell ({} · {})", s.host.hostname, s.host.chip),
        format!(
            "- Lid: {lid_label}, Power: {power_label}, Thermal: {} (CPU_Speed_Limit={}%)",
            s.thermal_state, s.cpu_speed_limit_pct
        ),
        format!(
            "- Active Policy: profile={} ({}), max_jobs={}, taskpolicy=\"{tp}\", nice={}",
            s.policy.profile, s.policy.mode_source, s.policy.max_jobs, s.policy.nice_level
        ),
        format!(
            "- Load Avg: {:.2} / {:.2} / {:.2}, Memory: {:.1}/{:.1} GB ({}%)",
            s.host.load_avg[0],
            s.host.load_avg[1],
            s.host.load_avg[2],
            s.host.memory.used_gb,
            s.host.memory.total_gb,
            s.host.memory.used_pct
        ),
    ];
    if s.display.available {
        let disp_desc = if s.display.mode == "saved_target" {
            format!(
                "lid closed (restore target {}%)",
                s.display.saved_percent.unwrap_or(0)
            )
        } else if s.display.asleep {
            format!(
                "asleep (target {}%)",
                s.display.saved_percent.or(s.display.percent).unwrap_or(0)
            )
        } else {
            format!("{}% (awake)", s.display.percent.unwrap_or(0))
        };
        lines.push(format!("- Built-in Display: {disp_desc}"));
    }
    lines.join("\n")
}

struct CachedEntry {
    at: Instant,
    gen: u64,
    snapshot: Arc<TelemetrySnapshot>,
}

struct InFlightSlot {
    gen: u64,
    result: Option<Arc<TelemetrySnapshot>>,
    aborted: bool,
}

struct LeaderCleanup<'a, P: ProbeBackend, D: DisplayBackend> {
    engine: &'a TelemetryEngine<P, D>,
    slot: Arc<(Mutex<InFlightSlot>, Condvar)>,
    completed: bool,
}

impl<P: ProbeBackend, D: DisplayBackend> Drop for LeaderCleanup<'_, P, D> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        {
            let mut guard = lock_unpoisoned(&self.engine.state);
            if guard
                .in_flight
                .as_ref()
                .map(|s| Arc::ptr_eq(s, &self.slot))
                .unwrap_or(false)
            {
                guard.in_flight = None;
            }
        }
        let (lock, cvar) = &*self.slot;
        lock_unpoisoned(lock).aborted = true;
        cvar.notify_all();
    }
}

struct Coalescer {
    cache: Option<CachedEntry>,
    in_flight: Option<Arc<(Mutex<InFlightSlot>, Condvar)>>,
}

pub struct TelemetryEngine<P: ProbeBackend, D: DisplayBackend> {
    data_dir: PathBuf,
    binary_path: String,
    probe: P,
    display: D,
    cache_ttl_ms: u64,
    settings_gen: AtomicU64,
    state: Mutex<Coalescer>,
}

impl<P: ProbeBackend, D: DisplayBackend> TelemetryEngine<P, D> {
    pub fn new(data_dir: PathBuf, binary_path: String, probe: P, display: D) -> Self {
        Self::with_ttl(data_dir, binary_path, probe, display, TELEMETRY_CACHE_TTL_MS)
    }

    pub fn with_ttl(
        data_dir: PathBuf,
        binary_path: String,
        probe: P,
        display: D,
        cache_ttl_ms: u64,
    ) -> Self {
        Self {
            data_dir,
            binary_path,
            probe,
            display,
            cache_ttl_ms,
            settings_gen: AtomicU64::new(0),
            state: Mutex::new(Coalescer {
                cache: None,
                in_flight: None,
            }),
        }
    }

    pub fn invalidate_cache(&self) {
        self.settings_gen.fetch_add(1, Ordering::SeqCst);
        let mut guard = lock_unpoisoned(&self.state);
        guard.cache = None;
    }

    pub fn prewarm(self: &Arc<Self>) {
        let _ = self.collect(true);
    }

    pub fn configure(&self, patch: &SettingsPatch) -> Result<CollectedTelemetry, String> {
        write_settings(&self.data_dir, patch)
            .map_err(|e| format!("failed to write settings: {e}"))?;
        self.invalidate_cache();
        Ok(self.collect(true))
    }

    pub fn set_brightness(
        &self,
        percent: f64,
        lid_override: Option<bool>,
    ) -> Result<(CollectedTelemetry, BrightnessAction), String> {
        let t0 = Instant::now();
        let action =
            set_brightness_synced(&self.data_dir, percent, lid_override, &self.probe, &self.display)?;
        self.invalidate_cache();
        let mut collected = self.collect(true);
        collected.server_ms = t0.elapsed().as_millis() as u64;
        Ok((collected, action))
    }

    fn run_leader(
        &self,
        req_gen: u64,
        slot: Arc<(Mutex<InFlightSlot>, Condvar)>,
    ) -> Arc<TelemetrySnapshot> {
        let mut cleanup = LeaderCleanup {
            engine: self,
            slot: Arc::clone(&slot),
            completed: false,
        };
        let snapshot = Arc::new(self.probe_fresh());
        {
            let mut guard = lock_unpoisoned(&self.state);
            // Generation-fenced cache update: never overwrite cache if settings/brightness changed!
            if self.settings_gen.load(Ordering::SeqCst) == req_gen {
                guard.cache = Some(CachedEntry {
                    at: Instant::now(),
                    gen: req_gen,
                    snapshot: Arc::clone(&snapshot),
                });
            }
            if guard
                .in_flight
                .as_ref()
                .map(|s| Arc::ptr_eq(s, &slot))
                .unwrap_or(false)
            {
                guard.in_flight = None;
            }
        }
        let (lock, cvar) = &*slot;
        lock_unpoisoned(lock).result = Some(Arc::clone(&snapshot));
        cleanup.completed = true;
        cvar.notify_all();
        snapshot
    }

    /// Stale-While-Revalidate (SWR) read for zero-block first paint (`resources/read`, `clamshell_panel`):
    /// - Fresh cache hit: returns immediately (`stale: false`, `0 ms`).
    /// - Stale cache hit (even hours after boot): returns cached snapshot immediately (`stale: true`,
    ///   `0 ms`) AND triggers a single-flight background refresh if none is running for `req_gen`.
    /// - Cold start (only before initial startup pre-warm completes): coalesces onto live `collect`.
    #[allow(dead_code)]
    pub fn collect_swr(self: &Arc<Self>) -> CollectedTelemetry
    where
        P: 'static,
        D: 'static,
    {
        let req_gen = self.settings_gen.load(Ordering::SeqCst);
        let mut bg_slot: Option<Arc<(Mutex<InFlightSlot>, Condvar)>> = None;
        let cached_hit: Option<(Arc<TelemetrySnapshot>, bool)> = {
            let mut guard = lock_unpoisoned(&self.state);
            let hit = guard.cache.as_ref().and_then(|c| {
                if c.gen == req_gen {
                    let fresh = (c.at.elapsed().as_millis() as u64) < self.cache_ttl_ms;
                    Some((Arc::clone(&c.snapshot), fresh))
                } else {
                    None
                }
            });
            if let Some((snapshot, fresh)) = hit {
                if !fresh {
                    let already_running = guard
                        .in_flight
                        .as_ref()
                        .map(|s| lock_unpoisoned(&s.0).gen == req_gen)
                        .unwrap_or(false);
                    if !already_running {
                        let slot = Arc::new((
                            Mutex::new(InFlightSlot {
                                gen: req_gen,
                                result: None,
                                aborted: false,
                            }),
                            Condvar::new(),
                        ));
                        guard.in_flight = Some(Arc::clone(&slot));
                        bg_slot = Some(slot);
                    }
                }
                Some((snapshot, !fresh))
            } else {
                None
            }
        };

        if let Some(slot) = bg_slot {
            let engine = Arc::clone(self);
            let slot_for_thread = Arc::clone(&slot);
            if std::thread::Builder::new()
                .name("clamshell-swr".into())
                .spawn(move || {
                    let _ = engine.run_leader(req_gen, slot_for_thread);
                })
                .is_err()
            {
                let mut guard = lock_unpoisoned(&self.state);
                if guard
                    .in_flight
                    .as_ref()
                    .map(|s| Arc::ptr_eq(s, &slot))
                    .unwrap_or(false)
                {
                    guard.in_flight = None;
                }
                let (lock, cvar) = &*slot;
                lock_unpoisoned(lock).aborted = true;
                cvar.notify_all();
            }
        }

        if let Some((snapshot, stale)) = cached_hit {
            return CollectedTelemetry {
                snapshot,
                server_ms: 0,
                from_cache: true,
                stale,
            };
        }

        self.collect(false)
    }

    /// Collect live or cached telemetry with strict single-flight coalescing & generation fencing.
    pub fn collect(&self, force: bool) -> CollectedTelemetry {
        let t0 = Instant::now();
        let req_gen = self.settings_gen.load(Ordering::SeqCst);

        let follower_slot: Option<Arc<(Mutex<InFlightSlot>, Condvar)>>;
        let leader_slot: Option<Arc<(Mutex<InFlightSlot>, Condvar)>>;

        {
            let mut guard = lock_unpoisoned(&self.state);
            if !force {
                if let Some(ref c) = guard.cache {
                    if c.gen == req_gen && (c.at.elapsed().as_millis() as u64) < self.cache_ttl_ms {
                        return CollectedTelemetry {
                            snapshot: Arc::clone(&c.snapshot),
                            server_ms: 0,
                            from_cache: true,
                            stale: false,
                        };
                    }
                }
            }

            // Strict single-flight coalescing: even concurrent `force=true` callers share the
            // currently running OS probe batch if settings_gen has not advanced.
            if let Some(ref existing) = guard.in_flight {
                let slot_gen = lock_unpoisoned(&existing.0).gen;
                if slot_gen == req_gen {
                    follower_slot = Some(Arc::clone(existing));
                    leader_slot = None;
                } else {
                    let slot = Arc::new((
                        Mutex::new(InFlightSlot {
                            gen: req_gen,
                            result: None,
                            aborted: false,
                        }),
                        Condvar::new(),
                    ));
                    guard.in_flight = Some(Arc::clone(&slot));
                    follower_slot = None;
                    leader_slot = Some(slot);
                }
            } else {
                let slot = Arc::new((
                    Mutex::new(InFlightSlot {
                        gen: req_gen,
                        result: None,
                        aborted: false,
                    }),
                    Condvar::new(),
                ));
                guard.in_flight = Some(Arc::clone(&slot));
                follower_slot = None;
                leader_slot = Some(slot);
            }
        }

        if let Some(slot) = follower_slot {
            let (lock, cvar) = &*slot;
            let mut st = lock_unpoisoned(lock);
            while st.result.is_none() && !st.aborted {
                st = cvar.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            if let Some(ref snap) = st.result {
                return CollectedTelemetry {
                    snapshot: Arc::clone(snap),
                    server_ms: t0.elapsed().as_millis() as u64,
                    from_cache: false,
                    stale: false,
                };
            }
            drop(st);
            // Leader aborted unexpectedly: run a direct fallback probe without panicking.
            return CollectedTelemetry {
                snapshot: Arc::new(self.probe_fresh()),
                server_ms: t0.elapsed().as_millis() as u64,
                from_cache: false,
                stale: false,
            };
        }

        let snapshot = if let Some(slot) = leader_slot {
            self.run_leader(req_gen, slot)
        } else {
            Arc::new(self.probe_fresh())
        };

        CollectedTelemetry {
            snapshot,
            server_ms: t0.elapsed().as_millis() as u64,
            from_cache: false,
            stale: false,
        }
    }

    pub fn refresh_json(&self, if_hash: Option<&str>, force: bool) -> (bool, Value, Arc<TelemetrySnapshot>) {
        let collected = self.collect(force);
        let clean_hash = if_hash.map(str::trim).unwrap_or("");
        if !clean_hash.is_empty() && clean_hash == collected.snapshot.hash {
            let payload = serde_json::to_value(UnchangedPayload {
                unchanged: true,
                hash: collected.snapshot.hash.clone(),
                timestamp: collected.snapshot.timestamp.clone(),
                server_ms: collected.server_ms,
                from_cache: collected.from_cache,
            })
            .unwrap_or_else(|_| json!({ "unchanged": true }));
            return (true, payload, collected.snapshot);
        }

        let mut val = serde_json::to_value(&*collected.snapshot).unwrap_or_else(|_| json!({}));
        if let Some(obj) = val.as_object_mut() {
            obj.insert("unchanged".into(), json!(false));
            obj.insert("serverMs".into(), json!(collected.server_ms));
            obj.insert("fromCache".into(), json!(collected.from_cache));
        }
        (false, val, collected.snapshot)
    }

    fn probe_fresh(&self) -> TelemetrySnapshot {
        let settings = read_settings(&self.data_dir);
        let guard_opt = load_guard_state_opt(&self.data_dir);
        let guard_for_disp = guard_opt.clone().unwrap_or_else(|| load_guard_state(&self.data_dir));
        let host_info = self.probe.static_host();
        let hw = self.probe.probe_hardware();
        let display = read_display_info(&self.display, &hw.lid_state, &guard_for_disp);
        let display_guard = guard_opt.as_ref().map(DisplayGuardSummary::from);

        let policy = compute_policy(
            &hw.lid_state,
            &hw.power_source,
            hw.battery_pct,
            &hw.thermal_state,
            &host_info.cpu_topology,
            &settings,
        );
        let env_vars = build_env_vars(&policy);

        let mut snap = TelemetrySnapshot {
            timestamp: format_iso8601_utc(now_epoch_ms()),
            host: HostTelemetry {
                hostname: host_info.hostname,
                chip: host_info.chip,
                macos_version: host_info.macos_version,
                uptime_seconds: hw.uptime_seconds,
                load_avg: hw.load_avg,
                memory: hw.memory,
            },
            lid_state: hw.lid_state,
            clamshell_causes_sleep: hw.clamshell_causes_sleep,
            power_source: hw.power_source,
            battery_pct: hw.battery_pct,
            battery_detail: hw.battery_detail,
            thermal_state: hw.thermal_state,
            cpu_speed_limit_pct: hw.cpu_speed_limit_pct,
            low_power_mode: hw.low_power_mode,
            cpu_topology: host_info.cpu_topology,
            settings,
            policy,
            env_vars,
            display,
            display_guard,
            degraded: hw.degraded,
            paths: TelemetryPaths {
                settings_path: abbreviate_home(&settings_path(&self.data_dir)),
                binary_path: abbreviate_home(Path::new(&self.binary_path)),
            },
            hash: String::new(),
        };
        snap.hash = compute_telemetry_hash(&snap);
        snap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::macos::{HardwareProbe, StaticHostInfo};
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    struct FakeProbe {
        delay_ms: u64,
        calls: Arc<AtomicUsize>,
        uptime: Arc<AtomicU64>,
        lid_closed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl ProbeBackend for FakeProbe {
        fn static_host(&self) -> StaticHostInfo {
            StaticHostInfo {
                hostname: "test-mac".into(),
                chip: "Apple M1 Pro".into(),
                macos_version: "15.4".into(),
                cpu_topology: CpuTopology {
                    total: 8,
                    performance_cores: 6,
                    efficiency_cores: 2,
                },
            }
        }
        fn lid_closed_fast(&self) -> Option<bool> {
            Some(self.lid_closed.load(Ordering::Relaxed))
        }
        fn probe_hardware(&self) -> HardwareProbe {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.delay_ms));
            }
            let closed = self.lid_closed.load(Ordering::Relaxed);
            HardwareProbe {
                lid_state: if closed { "closed" } else { "open" }.into(),
                clamshell_causes_sleep: "no".into(),
                power_source: "battery".into(),
                battery_pct: Some(85),
                battery_detail: "-InternalBattery-0\t85%; discharging".into(),
                thermal_state: "ok".into(),
                cpu_speed_limit_pct: 100,
                low_power_mode: "0".into(),
                memory: MemorySummary {
                    total_gb: 16.0,
                    used_gb: 8.5,
                    compressed_gb: 1.2,
                    used_pct: 53,
                },
                load_avg: [1.5, 1.25, 1.1],
                uptime_seconds: self.uptime.load(Ordering::Relaxed),
                degraded: false,
            }
        }
    }

    struct FakeDisplay {
        brightness: Arc<Mutex<f32>>,
    }

    impl DisplayBackend for FakeDisplay {
        fn get_displays(&self) -> (Vec<u32>, Vec<u32>) {
            (vec![1], vec![])
        }
        fn is_asleep(&self, _id: u32) -> Option<bool> {
            Some(false)
        }
        fn get_brightness(&self, _id: u32) -> Option<f32> {
            Some(*self.brightness.lock().unwrap())
        }
        fn set_brightness(&self, _id: u32, level: f32) -> bool {
            *self.brightness.lock().unwrap() = level;
            true
        }
        fn sleep_display_now(&self) -> bool {
            true
        }
    }

    #[test]
    fn etag_dedup_single_flight_and_generation_fencing() {
        let dir =
            std::env::temp_dir().join(format!("mac-clamshell-tel-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let calls = Arc::new(AtomicUsize::new(0));
        let uptime = Arc::new(AtomicU64::new(3600));
        let lid_closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let bright = Arc::new(Mutex::new(0.70f32));

        let engine = Arc::new(TelemetryEngine::with_ttl(
            dir.clone(),
            "mac-clamshell-mcp".into(),
            FakeProbe {
                delay_ms: 40,
                calls: Arc::clone(&calls),
                uptime: Arc::clone(&uptime),
                lid_closed: Arc::clone(&lid_closed),
            },
            FakeDisplay {
                brightness: Arc::clone(&bright),
            },
            2500,
        ));

        // 1. 8 concurrent force=true calls coalesce into EXACTLY 1 probe!
        let mut handles = Vec::new();
        for _ in 0..8 {
            let e = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || e.collect(true)));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let h0 = results[0].snapshot.hash.clone();
        assert_eq!(h0.len(), 16);

        // 2. Sub-minute uptime tick + forced refresh still returns unchanged: true
        uptime.store(3615, Ordering::Relaxed);
        let (unchanged, payload, _) = engine.refresh_json(Some(&h0), true);
        assert!(unchanged);
        assert_eq!(payload["unchanged"], true);
        assert!(payload.get("host").is_none());

        // 3. Brightness update changes hash and returns live brightness
        let (upd, act) = engine.set_brightness(45.0, Some(false)).unwrap();
        assert!(act.applied);
        assert_eq!(upd.snapshot.display.percent, Some(45));
        assert_ne!(upd.snapshot.hash, h0);

        // 4. Generation fencing during in-flight collect
        let e1 = Arc::clone(&engine);
        let t_old = std::thread::spawn(move || e1.collect(true));
        std::thread::sleep(Duration::from_millis(10));
        let cfg_res = engine
            .configure(&SettingsPatch {
                mode_override: Some("strict".into()),
                max_jobs_override: None,
                ac_closed_profile: None,
            })
            .unwrap();
        let old_res = t_old.join().unwrap();
        assert_eq!(old_res.snapshot.policy.profile, "normal");
        assert_eq!(cfg_res.snapshot.policy.profile, "strict");

        // Subsequent cached collect MUST see strict, never stale normal!
        let cached = engine.collect(false);
        assert!(cached.from_cache);
        assert_eq!(cached.snapshot.policy.profile, "strict");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swr_returns_stale_immediately_and_revalidates_in_background() {
        let dir =
            std::env::temp_dir().join(format!("mac-clamshell-swr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let calls = Arc::new(AtomicUsize::new(0));
        let uptime = Arc::new(AtomicU64::new(3600));
        let lid_closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let bright = Arc::new(Mutex::new(0.70f32));

        let engine = Arc::new(TelemetryEngine::with_ttl(
            dir.clone(),
            "mac-clamshell-mcp".into(),
            FakeProbe {
                delay_ms: 45,
                calls: Arc::clone(&calls),
                uptime: Arc::clone(&uptime),
                lid_closed: Arc::clone(&lid_closed),
            },
            FakeDisplay {
                brightness: Arc::clone(&bright),
            },
            5,
        ));

        // 1. Pre-warm at startup
        engine.prewarm();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 2. Fresh hit returns immediately with stale=false
        let fresh = engine.collect_swr();
        assert!(fresh.from_cache);
        assert!(!fresh.stale);
        assert_eq!(fresh.snapshot.lid_state, "open");

        // 3. Let cache expire (simulating hours idle) & flip hardware lid to closed
        std::thread::sleep(Duration::from_millis(12));
        lid_closed.store(true, Ordering::Relaxed);

        // 4. Two back-to-back SWR calls (resources/read + clamshell_panel) MUST return in <5ms
        //    without blocking on the 45ms probe, and trigger ONLY ONE background probe!
        let t0 = Instant::now();
        let swr1 = engine.collect_swr();
        let swr2 = engine.collect_swr();
        let elapsed_ms = t0.elapsed().as_millis();
        assert!(elapsed_ms < 15, "SWR blocked for {elapsed_ms}ms");
        assert!(swr1.from_cache && swr1.stale);
        assert!(swr2.from_cache && swr2.stale);
        assert_eq!(swr1.snapshot.lid_state, "open");
        assert_eq!(swr2.snapshot.lid_state, "open");

        // 5. Follow-up collect(false) coalesces onto the single background probe and gets fresh closed state
        let after = engine.collect(false);
        assert_eq!(after.snapshot.lid_state, "closed");
        assert!(!after.stale);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
