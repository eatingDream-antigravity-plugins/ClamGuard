use crate::platform::display_ffi::{
    clamp_percent, level_f64_to_percent, level_to_percent, DisplayBackend, MacOsDisplayBackend,
};
use crate::platform::macos::{MacOsProbeBackend, ProbeBackend};
use crate::state::{
    load_guard_state, now_epoch_secs, read_settings, save_guard_state, FileLock, GuardState,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const MIN_SLEEP_DEBOUNCE_SECS: f64 = 15.0;
pub const FALLBACK_REENFORCE_SECS: f64 = 300.0;
pub const MAX_RESTORE_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    pub available: bool,
    pub asleep: bool,
    pub percent: Option<u8>,
    pub saved_percent: Option<u8>,
    pub builtin_count: usize,
    pub external_count: usize,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayGuardSummary {
    pub last_lid: String,
    pub saved_brightness: Option<f64>,
    pub last_sleep_ts: f64,
    pub needs_restore: bool,
    pub user_target: bool,
    pub thermal_capped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_thermal_brightness: Option<f64>,
}

impl From<&GuardState> for DisplayGuardSummary {
    fn from(s: &GuardState) -> Self {
        Self {
            last_lid: s.last_lid.clone(),
            saved_brightness: if s.saved_brightness.is_finite() {
                Some((s.saved_brightness * 1000.0).round() / 1000.0)
            } else {
                None
            },
            last_sleep_ts: s.last_sleep_ts,
            needs_restore: s.needs_restore,
            user_target: s.user_target,
            thermal_capped: s.thermal_capped,
            pre_thermal_brightness: s
                .pre_thermal_brightness
                .filter(|v| v.is_finite())
                .map(|v| (v * 1000.0).round() / 1000.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrightnessAction {
    pub requested_percent: u8,
    pub applied: bool,
    pub deferred: bool,
    pub mode: String,
    pub percent: Option<u8>,
    pub saved_percent: u8,
    pub asleep: bool,
}

pub fn read_display_info(
    display: &dyn DisplayBackend,
    lid_state: &str,
    guard_state: &GuardState,
) -> DisplayInfo {
    let closed = lid_state == "closed";
    let (builtins, externals) = display.get_displays();
    let (cur_pct, hw_asleep) = if let Some(&primary) = builtins.first() {
        let cur = display.get_brightness(primary).and_then(level_to_percent);
        let slp = display.is_asleep(primary).unwrap_or(false);
        (cur, slp)
    } else {
        (None, closed)
    };

    let mut saved_percent = level_f64_to_percent(guard_state.saved_brightness);
    if let Ok(mock) = std::env::var("MAC_CLAMSHELL_MOCK_BRIGHTNESS") {
        if let Ok(v) = mock.trim().parse::<f64>() {
            if let Ok(pct) = clamp_percent(v) {
                if !guard_state.user_target
                    && !guard_state.needs_restore
                    && guard_state.last_lid == "open"
                    && (guard_state.saved_brightness - 0.20).abs() < 1e-6
                {
                    saved_percent = Some(pct);
                }
            }
        }
    }

    // In docked clamshell mode (lid closed + external monitor), macOS WindowServer takes the
    // internal panel offline (`builtins.is_empty()`), while `lid_state == "closed"` confirms
    // this is a clamshell MacBook whose saved restore target remains valid and editable.
    let available = closed || (!builtins.is_empty() && (cur_pct.is_some() || hw_asleep));
    let asleep = if closed { true } else { hw_asleep };
    let percent = if closed {
        Some(cur_pct.unwrap_or(0))
    } else {
        cur_pct
    };
    let mode = if !available {
        "unavailable"
    } else if closed {
        "saved_target"
    } else if asleep {
        "asleep"
    } else {
        "live"
    }
    .to_string();

    DisplayInfo {
        available,
        asleep,
        percent,
        saved_percent,
        builtin_count: builtins.len(),
        external_count: externals.len(),
        mode,
    }
}

/// Apply user-requested brightness (0..100%) safely and keep Display Guard restore state synced.
pub fn apply_user_brightness(
    percent: f64,
    closed: Option<bool>,
    state: &GuardState,
    display: &dyn DisplayBackend,
) -> Result<(GuardState, bool, BrightnessAction), String> {
    let target_pct = clamp_percent(percent)?;
    let level_f64 = ((target_pct as f64) / 100.0 * 1000.0).round() / 1000.0;
    let level_f32 = level_f64 as f32;
    let (builtins, _externals) = display.get_displays();
    let mut next = state.clone();

    let asleep = !builtins.is_empty()
        && builtins
            .iter()
            .all(|&d| display.is_asleep(d).unwrap_or(false));
    let cur_pct = builtins
        .first()
        .and_then(|&d| display.get_brightness(d))
        .and_then(level_to_percent);

    if closed == Some(true) {
        // Thermal safety: never raise built-in backlight while lid is closed; persist restore target.
        next.saved_brightness = level_f64;
        next.user_target = true;
        next.needs_restore = target_pct > 0;
        next.last_lid = "closed".to_string();
        next.restore_attempts = 0;
        next.thermal_capped = false;
        next.pre_thermal_brightness = None;
        let changed = next != *state;
        return Ok((
            next,
            changed,
            BrightnessAction {
                requested_percent: target_pct,
                applied: false,
                deferred: true,
                mode: "saved_target".to_string(),
                percent: Some(cur_pct.unwrap_or(0)),
                saved_percent: target_pct,
                asleep: true,
            },
        ));
    }

    if builtins.is_empty() {
        return Err("no built-in display detected on this Mac".to_string());
    }

    if asleep {
        let applied = builtins.iter().all(|&d| display.set_brightness(d, level_f32));
        next.saved_brightness = level_f64;
        next.user_target = true;
        next.needs_restore = target_pct > 0;
        next.last_lid = "open".to_string();
        next.restore_attempts = 0;
        next.thermal_capped = false;
        next.pre_thermal_brightness = None;
        let changed = next != *state;
        return Ok((
            next,
            changed,
            BrightnessAction {
                requested_percent: target_pct,
                applied,
                deferred: true,
                mode: "saved_target".to_string(),
                percent: Some(cur_pct.unwrap_or(0)),
                saved_percent: target_pct,
                asleep: true,
            },
        ));
    }

    // Lid is open and built-in display is awake -> apply immediately; surface real FFI errors.
    let applied = builtins.iter().all(|&d| display.set_brightness(d, level_f32));
    if !applied {
        return Err("failed to set built-in display brightness via DisplayServices".to_string());
    }

    let actual_pct = builtins
        .first()
        .and_then(|&d| display.get_brightness(d))
        .and_then(level_to_percent);
    let reported_pct = match actual_pct {
        Some(a) if a.abs_diff(target_pct) <= 1 => Some(target_pct),
        Some(a) => Some(a),
        None => Some(target_pct),
    };

    next.saved_brightness = level_f64;
    next.user_target = false;
    next.needs_restore = false;
    next.restore_attempts = 0;
    next.last_lid = "open".to_string();
    next.thermal_capped = false;
    next.pre_thermal_brightness = None;
    let changed = next != *state;

    Ok((
        next,
        changed,
        BrightnessAction {
            requested_percent: target_pct,
            applied: true,
            deferred: false,
            mode: "applied".to_string(),
            percent: reported_pct,
            saved_percent: target_pct,
            asleep: false,
        },
    ))
}

/// Execute one deterministic Display Guard tick (closed-lid sleep/zero + wake-restore + optional thermal cap).
pub fn evaluate_guard_tick_with_thermal(
    closed: Option<bool>,
    thermal_pressure: &str,
    thermal_cap_pct: u8,
    state: &GuardState,
    now: f64,
    display: &dyn DisplayBackend,
) -> (GuardState, bool) {
    // Fail-safe: on transient ioreg failure/timeout or desktop Mac, never flip lid state or touch brightness.
    let Some(is_closed) = closed else {
        return (state.clone(), false);
    };

    let mut next = state.clone();
    let prev_lid = next.last_lid.clone();
    let needs_restore = next.needs_restore;
    let high_thermal = matches!(thermal_pressure, "serious" | "critical");
    let want_thermal_cap = !is_closed && high_thermal && (1..=100).contains(&thermal_cap_pct);

    // Fast path: steady-state open, nothing pending restore, and no thermal brightness cap work -> skip CoreGraphics FFI.
    if !is_closed && prev_lid == "open" && !needs_restore && !want_thermal_cap && !next.thermal_capped
    {
        return (next, false);
    }

    let (builtins, externals) = display.get_displays();

    if is_closed {
        let was_closed = prev_lid == "closed";
        let pre_cap_restore = if next.thermal_capped {
            next.pre_thermal_brightness.filter(|v| v.is_finite() && *v > 0.01)
        } else {
            None
        };
        next.thermal_capped = false;
        next.pre_thermal_brightness = None;

        for &did in &builtins {
            if let Some(cur) = display.get_brightness(did) {
                if cur > 0.01 {
                    let rounded = pre_cap_restore
                        .unwrap_or_else(|| ((cur as f64) * 1000.0).round() / 1000.0);
                    if !was_closed {
                        next.saved_brightness = rounded;
                        next.user_target = false;
                    } else if !next.user_target && next.saved_brightness <= 0.0 {
                        next.saved_brightness = rounded;
                    }
                    if display.set_brightness(did, 0.0) {
                        next.needs_restore = true;
                    }
                }
            }
        }
        if builtins.is_empty() && pre_cap_restore.is_some() && !was_closed {
            next.saved_brightness = pre_cap_restore.unwrap();
        }

        if externals.is_empty() {
            let elapsed = (now - next.last_sleep_ts).max(0.0);
            let any_awake = if !builtins.is_empty() {
                builtins
                    .iter()
                    .any(|&d| display.is_asleep(d) != Some(true))
            } else {
                elapsed >= FALLBACK_REENFORCE_SECS
            };
            let should_sleep = !was_closed || (any_awake && elapsed >= MIN_SLEEP_DEBOUNCE_SECS);
            if should_sleep {
                display.sleep_display_now();
                next.last_sleep_ts = now;
            }
        }

        next.last_lid = "closed".to_string();
        next.restore_attempts = 0;
        let changed = next != *state;
        return (next, changed);
    }

    // Lid is OPEN and either just transitioned from closed or retrying wake restore.
    if prev_lid == "closed" || needs_restore {
        let user_target = next.user_target;
        let saved = if next.saved_brightness.is_finite() {
            next.saved_brightness
        } else {
            0.20
        };
        let desired = if user_target {
            saved.clamp(0.0, 1.0)
        } else {
            saved.clamp(0.15, 1.0)
        };
        let cap_f64 = (thermal_cap_pct as f64) / 100.0;
        let target = if want_thermal_cap && desired > cap_f64 {
            next.pre_thermal_brightness = Some(desired);
            next.thermal_capped = true;
            cap_f64 as f32
        } else {
            desired as f32
        };

        let attempts = next.restore_attempts.saturating_add(1);
        let restored_ok = if user_target && target < 0.01 {
            true
        } else if !builtins.is_empty() {
            let mut all_ok = true;
            for &did in &builtins {
                let cur = display.get_brightness(did);
                if !user_target && !want_thermal_cap && cur.map(|c| c >= 0.02).unwrap_or(false) {
                    continue;
                }
                if (user_target || want_thermal_cap)
                    && cur.map(|c| (c - target).abs() <= 0.02).unwrap_or(false)
                {
                    continue;
                }
                if !display.set_brightness(did, target) {
                    all_ok = false;
                }
            }
            all_ok
        } else {
            false
        };

        if restored_ok || attempts >= MAX_RESTORE_ATTEMPTS {
            next.last_lid = "open".to_string();
            next.needs_restore = false;
            next.restore_attempts = 0;
            next.user_target = false;
        } else {
            next.last_lid = "open".to_string();
            next.needs_restore = true;
            next.restore_attempts = attempts;
        }
        let changed = next != *state;
        return (next, changed);
    }

    // Steady-state open lid: enforce or lift optional thermal brightness cap.
    if want_thermal_cap {
        let cap_level = ((thermal_cap_pct as f32) / 100.0).clamp(0.01, 1.0);
        for &did in &builtins {
            if display.is_asleep(did) == Some(true) {
                continue;
            }
            if let Some(cur) = display.get_brightness(did) {
                if cur > cap_level + 0.015 {
                    if !next.thermal_capped {
                        next.pre_thermal_brightness =
                            Some(((cur as f64) * 1000.0).round() / 1000.0);
                    }
                    if display.set_brightness(did, cap_level) {
                        next.thermal_capped = true;
                    }
                }
            }
        }
    } else if next.thermal_capped {
        let restore_level = next
            .pre_thermal_brightness
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(next.saved_brightness)
            .clamp(0.05, 1.0) as f32;
        let mut all_ok = true;
        for &did in &builtins {
            if display.is_asleep(did) == Some(true) {
                continue;
            }
            if !display.set_brightness(did, restore_level) {
                all_ok = false;
            }
        }
        if all_ok || builtins.is_empty() {
            next.thermal_capped = false;
            next.pre_thermal_brightness = None;
        }
    }

    next.last_lid = "open".to_string();
    next.needs_restore = false;
    next.restore_attempts = 0;
    let changed = next != *state;
    (next, changed)
}

#[cfg(test)]
pub fn evaluate_guard_tick(
    closed: Option<bool>,
    state: &GuardState,
    now: f64,
    display: &dyn DisplayBackend,
) -> (GuardState, bool) {
    evaluate_guard_tick_with_thermal(closed, "nominal", 0, state, now, display)
}

pub fn set_brightness_synced(
    data_dir: &Path,
    percent: f64,
    lid_override: Option<bool>,
    probe: &dyn ProbeBackend,
    display: &dyn DisplayBackend,
) -> Result<BrightnessAction, String> {
    let closed = match lid_override {
        Some(v) => Some(v),
        None => probe.lid_closed_fast(),
    };
    let _lock = FileLock::acquire(data_dir, true)
        .map_err(|e| format!("failed to acquire display-guard lock: {e}"))?;
    let state = load_guard_state(data_dir);
    let (next, changed, action) = apply_user_brightness(percent, closed, &state, display)?;
    if changed {
        save_guard_state(data_dir, &next)
            .map_err(|e| format!("failed to persist display-guard state: {e}"))?;
    }
    Ok(action)
}

/// Run one lock-coordinated Display Guard tick if `settings.guard_enabled` is true.
/// Returns `Ok(None)` when paused via settings, `Ok(Some(false))` when another guard holds the lock
/// or state did not change, and `Ok(Some(true))` when guard state changed on disk.
pub fn run_guard_tick_locked(
    data_dir: &Path,
    probe: &dyn ProbeBackend,
    display: &dyn DisplayBackend,
) -> Result<Option<bool>, String> {
    let settings = read_settings(data_dir);
    if !settings.guard_enabled {
        return Ok(None);
    }
    let Ok(Some(_lock)) = FileLock::acquire(data_dir, false) else {
        return Ok(Some(false));
    };
    let closed = probe.lid_closed_fast();
    let pressure = probe.thermal_pressure_fast();
    let state = load_guard_state(data_dir);
    let (next, changed) = evaluate_guard_tick_with_thermal(
        closed,
        &pressure,
        settings.thermal_bright_cap_pct,
        &state,
        now_epoch_secs(),
        display,
    );
    if changed {
        save_guard_state(data_dir, &next)
            .map_err(|e| format!("failed to persist display-guard state: {e}"))?;
    }
    Ok(Some(changed))
}

pub fn run_watch_once(data_dir: &Path) -> Result<(), String> {
    let probe = MacOsProbeBackend;
    let display = MacOsDisplayBackend;
    let _ = run_guard_tick_locked(data_dir, &probe, &display)?;
    Ok(())
}

static STOP_DAEMON: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    STOP_DAEMON.store(true, Ordering::Relaxed);
}

pub fn run_watch_daemon(data_dir: &Path, interval_secs: u64) -> Result<(), String> {
    STOP_DAEMON.store(false, Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGINT, handle_signal as *const () as usize);
        libc::signal(libc::SIGTERM, handle_signal as *const () as usize);
    }
    let sleep_dur = Duration::from_secs(interval_secs.clamp(2, 300));
    while !STOP_DAEMON.load(Ordering::Relaxed) {
        let _ = run_watch_once(data_dir);
        let steps = (sleep_dur.as_millis() / 250).max(1);
        for _ in 0..steps {
            if STOP_DAEMON.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockDisplay {
        builtins: Mutex<Vec<u32>>,
        externals: Mutex<Vec<u32>>,
        asleep: Mutex<bool>,
        brightness: Mutex<Option<f32>>,
        fail_set: Mutex<bool>,
        writes: Mutex<Vec<(u32, f32)>>,
        sleeps: Mutex<u32>,
        display_calls: Mutex<u32>,
    }

    impl MockDisplay {
        fn new(brightness: f32, asleep: bool) -> Self {
            Self {
                builtins: Mutex::new(vec![1]),
                externals: Mutex::new(vec![]),
                asleep: Mutex::new(asleep),
                brightness: Mutex::new(Some(brightness)),
                fail_set: Mutex::new(false),
                writes: Mutex::new(vec![]),
                sleeps: Mutex::new(0),
                display_calls: Mutex::new(0),
            }
        }
    }

    impl DisplayBackend for MockDisplay {
        fn get_displays(&self) -> (Vec<u32>, Vec<u32>) {
            *self.display_calls.lock().unwrap() += 1;
            (
                self.builtins.lock().unwrap().clone(),
                self.externals.lock().unwrap().clone(),
            )
        }
        fn is_asleep(&self, _id: u32) -> Option<bool> {
            Some(*self.asleep.lock().unwrap())
        }
        fn get_brightness(&self, _id: u32) -> Option<f32> {
            *self.brightness.lock().unwrap()
        }
        fn set_brightness(&self, id: u32, level: f32) -> bool {
            if *self.fail_set.lock().unwrap() {
                return false;
            }
            self.writes.lock().unwrap().push((id, level));
            *self.brightness.lock().unwrap() = Some(level);
            true
        }
        fn sleep_display_now(&self) -> bool {
            *self.sleeps.lock().unwrap() += 1;
            *self.asleep.lock().unwrap() = true;
            true
        }
    }

    #[test]
    fn guard_fail_safe_fast_path_and_user_target_wake_restore() {
        let disp = MockDisplay::new(0.5, false);

        // 1. Fail-safe on unknown lid (ioreg timeout)
        let closed_st = GuardState {
            last_lid: "closed".into(),
            saved_brightness: 0.8,
            last_sleep_ts: 100.0,
            needs_restore: true,
            restore_attempts: 0,
            user_target: false,
            ..GuardState::default()
        };
        let (s1, changed) = evaluate_guard_tick(None, &closed_st, 110.0, &disp);
        assert!(!changed);
        assert_eq!(s1, closed_st);
        assert_eq!(*disp.display_calls.lock().unwrap(), 0);

        // 2. Steady-state open fast path skips CoreGraphics completely
        let open_st = GuardState::default();
        let (_, changed) = evaluate_guard_tick(Some(false), &open_st, 200.0, &disp);
        assert!(!changed);
        assert_eq!(*disp.display_calls.lock().unwrap(), 0);

        // 3. Open + awake user brightness sets immediately & updates saved_brightness
        let (st_u1, changed, act1) =
            apply_user_brightness(65.0, Some(false), &open_st, &disp).unwrap();
        assert!(changed);
        assert!(act1.applied);
        assert_eq!(act1.mode, "applied");
        assert_eq!(act1.percent, Some(65));
        assert_eq!(st_u1.saved_brightness, 0.65);

        // 4. Open -> Closed captures 0.65, zeroes backlight, sleeps display
        let (st_c1, changed) = evaluate_guard_tick(Some(true), &st_u1, 300.0, &disp);
        assert!(changed);
        assert_eq!(st_c1.last_lid, "closed");
        assert_eq!(st_c1.saved_brightness, 0.65);
        assert_eq!(*disp.brightness.lock().unwrap(), Some(0.0));
        assert_eq!(*disp.sleeps.lock().unwrap(), 1);

        // 5. While closed, user sets brightness target to 10% -> zero backlight writes, saves user_target
        disp.writes.lock().unwrap().clear();
        let (st_c2, changed, act2) =
            apply_user_brightness(10.0, Some(true), &st_c1, &disp).unwrap();
        assert!(changed);
        assert!(disp.writes.lock().unwrap().is_empty());
        assert_eq!(act2.mode, "saved_target");
        assert!(act2.deferred);
        assert_eq!(act2.saved_percent, 10);
        assert!(st_c2.user_target);

        // 6. Steady-state closed tick with asleep display -> NO disk write, NO extra pmset
        let (st_c3, changed) = evaluate_guard_tick(Some(true), &st_c2, 320.0, &disp);
        assert!(!changed);
        assert_eq!(st_c3.saved_brightness, 0.1);
        assert_eq!(*disp.sleeps.lock().unwrap(), 1);

        // 7. Closed -> Open wake-race: tick 1 empty builtins retries on tick 2 and restores exact 10%
        disp.builtins.lock().unwrap().clear();
        let (st_r1, changed) = evaluate_guard_tick(Some(false), &st_c2, 400.0, &disp);
        assert!(changed);
        assert!(st_r1.needs_restore);
        assert_eq!(st_r1.restore_attempts, 1);

        *disp.builtins.lock().unwrap() = vec![1];
        *disp.asleep.lock().unwrap() = false;
        let (st_r2, changed) = evaluate_guard_tick(Some(false), &st_r1, 410.0, &disp);
        assert!(changed);
        assert!(!st_r2.needs_restore);
        assert!(!st_r2.user_target);
        assert!((*disp.brightness.lock().unwrap()).unwrap() - 0.1 < 0.001);
    }

    #[test]
    fn desktop_mac_error_docked_clamshell_and_thermal_cap_restore() {
        let disp = MockDisplay::new(0.85, false);

        // 1. Desktop Mac (no builtins, lid != closed) returns explicit error on brightness set
        disp.builtins.lock().unwrap().clear();
        *disp.externals.lock().unwrap() = vec![2];
        let st0 = GuardState {
            saved_brightness: 0.75,
            ..GuardState::default()
        };
        let desktop_info = read_display_info(&disp, "unknown", &st0);
        assert!(!desktop_info.available);
        assert_eq!(desktop_info.mode, "unavailable");
        assert!(apply_user_brightness(50.0, None, &st0, &disp).is_err());

        // 2. Docked closed-lid MacBook (builtins offline, lid == closed) reports available saved_target
        let docked_info = read_display_info(&disp, "closed", &st0);
        assert!(docked_info.available);
        assert_eq!(docked_info.mode, "saved_target");
        assert_eq!(docked_info.saved_percent, Some(75));
        assert_eq!(docked_info.external_count, 1);

        // 3. Open awake display where DisplayServicesSetBrightness fails -> explicit error (not fake saved_target)
        *disp.builtins.lock().unwrap() = vec![1];
        *disp.externals.lock().unwrap() = vec![];
        *disp.fail_set.lock().unwrap() = true;
        assert!(apply_user_brightness(40.0, Some(false), &st0, &disp).is_err());
        *disp.fail_set.lock().unwrap() = false;

        // 4. Thermal brightness cap under serious pressure + automatic restore on cool-down
        let (st_cap, changed) =
            evaluate_guard_tick_with_thermal(Some(false), "serious", 40, &st0, 500.0, &disp);
        assert!(changed);
        assert!(st_cap.thermal_capped);
        assert_eq!(st_cap.pre_thermal_brightness, Some(0.85));
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.40).abs() < 0.005);

        let (st_cool, changed) =
            evaluate_guard_tick_with_thermal(Some(false), "nominal", 40, &st_cap, 520.0, &disp);
        assert!(changed);
        assert!(!st_cool.thermal_capped);
        assert_eq!(st_cool.pre_thermal_brightness, None);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.85).abs() < 0.005);
    }
}
