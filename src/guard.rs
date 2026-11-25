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
pub const MAX_RESTORE_ATTEMPTS: u32 = 12;
pub const POST_RESTORE_WATCH_SECS: f64 = 6.0;
pub const OPEN_SYNC_EPSILON: f64 = 0.015;
pub const FAST_POLL_INTERVAL_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardTickOutcome {
    pub paused: bool,
    pub changed: bool,
    pub fast_poll: bool,
    pub open_restoring: bool,
}

pub fn in_post_restore_watch(state: &GuardState, now: f64) -> bool {
    state.last_restore_ts > 0.0
        && (now - state.last_restore_ts) >= 0.0
        && (now - state.last_restore_ts) <= POST_RESTORE_WATCH_SECS
}

pub fn guard_wants_fast_poll(state: &GuardState, now: f64) -> bool {
    state.last_lid == "closed" || state.needs_restore || in_post_restore_watch(state, now)
}

pub fn guard_is_open_restoring(state: &GuardState, now: f64) -> bool {
    state.last_lid == "open" && (state.needs_restore || in_post_restore_watch(state, now))
}

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
        next.last_restore_ts = 0.0;
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
        next.last_restore_ts = 0.0;
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
    next.last_restore_ts = 0.0;
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
        next.last_restore_ts = 0.0;

        let mut dimmed_any = false;
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
                        dimmed_any = true;
                    }
                }
            }
        }
        if !dimmed_any && !was_closed {
            if let Some(pre) = pre_cap_restore {
                next.saved_brightness = pre;
                next.user_target = false;
                next.needs_restore = true;
            }
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
        } else if saved > 0.01 {
            saved.clamp(0.05, 1.0)
        } else {
            0.20
        };
        let cap_f64 = (thermal_cap_pct as f64) / 100.0;
        let apply_cap = want_thermal_cap && desired > cap_f64;
        let target = if apply_cap {
            cap_f64 as f32
        } else {
            desired as f32
        };

        let mut any_asleep = false;
        let mut wrote_any = false;
        let restored_ok = if user_target && target < 0.01 {
            true
        } else if !builtins.is_empty() {
            let mut all_ok = true;
            for &did in &builtins {
                if display.is_asleep(did) == Some(true) {
                    any_asleep = true;
                    all_ok = false;
                    continue;
                }
                let cur = display.get_brightness(did);
                // When we dimmed the screen (`needs_restore`), or user set a closed-lid target,
                // or thermal capping is active, require brightness to match `target` rather than
                // skipping on any `cur >= 0.02` (which left the screen dim after Auto-Brightness
                // or a single F2 tap woke it at 3-10%).
                let match_tol = (target * 0.4).clamp(0.005, 0.02);
                let already_ok = if needs_restore || user_target || apply_cap {
                    cur.map(|c| (c - target).abs() <= match_tol).unwrap_or(false)
                } else {
                    // We never dimmed this display while closed; if macOS woke it to a visible
                    // level on its own, keep and sync that level instead of overriding it.
                    cur.map(|c| c >= 0.02).unwrap_or(false)
                };
                if already_ok {
                    if !needs_restore && !user_target && !apply_cap {
                        if let Some(c) = cur {
                            next.saved_brightness = ((c as f64) * 1000.0).round() / 1000.0;
                        }
                    }
                    continue;
                }
                if !display.set_brightness(did, target) {
                    all_ok = false;
                    continue;
                }
                wrote_any = true;
                // Read back to catch writes silently ignored during WindowServer's wake transition.
                // Scale tolerance with target so dropped writes to 0.0 are caught even at the 5% floor.
                let tol = (target * 0.4).clamp(0.005, 0.05);
                if let Some(after) = display.get_brightness(did) {
                    if (after - target).abs() > tol {
                        all_ok = false;
                    }
                }
            }
            all_ok
        } else {
            false
        };

        if restored_ok {
            if apply_cap {
                next.pre_thermal_brightness = Some(desired);
                next.thermal_capped = true;
            }
            next.last_lid = "open".to_string();
            next.needs_restore = false;
            next.restore_attempts = 0;
            next.user_target = false;
            if wrote_any && target >= 0.05 {
                next.last_restore_ts = now;
            } else {
                next.last_restore_ts = 0.0;
            }
        } else if any_asleep {
            // Built-in panel is still asleep after lid open: stay pending without exhausting retries.
            next.last_lid = "open".to_string();
            next.needs_restore = true;
        } else {
            let attempts = next.restore_attempts.saturating_add(1);
            if attempts >= MAX_RESTORE_ATTEMPTS {
                next.last_lid = "open".to_string();
                next.needs_restore = false;
                next.restore_attempts = 0;
                next.user_target = false;
            } else {
                next.last_lid = "open".to_string();
                next.needs_restore = true;
                next.restore_attempts = attempts;
            }
        }
        let changed = next != *state;
        return (next, changed);
    }

    // Steady-state open lid: enforce or lift optional thermal brightness cap.
    if want_thermal_cap {
        let cap_level = ((thermal_cap_pct as f32) / 100.0).clamp(0.01, 1.0);
        let clobber_watch = in_post_restore_watch(&next, now);
        let saved_target = if next.saved_brightness.is_finite() && next.saved_brightness > 0.01 {
            next.saved_brightness.clamp(0.05, 1.0) as f32
        } else {
            0.20
        };
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
                        next.last_restore_ts = 0.0;
                    }
                } else if clobber_watch && cur < 0.02 {
                    let _ = display.set_brightness(did, saved_target.min(cap_level));
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
    } else if in_post_restore_watch(&next, now) {
        // Post-wake clobber watch: macOS loginwindow / CoreBrightness can reset brightness back
        // to ~0% 1-3s after Display is turned on. Re-apply saved brightness if it drops to black
        // (<2%), while leaving any normal manual adjustment (>=2%) untouched.
        let saved = if next.saved_brightness.is_finite() && next.saved_brightness > 0.01 {
            next.saved_brightness.clamp(0.05, 1.0)
        } else {
            0.20
        };
        let target = saved as f32;
        for &did in &builtins {
            if display.is_asleep(did) == Some(true) {
                continue;
            }
            if let Some(cur) = display.get_brightness(did) {
                if cur < 0.02 {
                    let _ = display.set_brightness(did, target);
                }
            }
        }
    } else if let Some(&primary) = builtins.first() {
        // Keep `saved_brightness` fresh while open & awake so keyboard/Control Center adjustments
        // are preserved even if macOS fades or disconnects the panel before our closed-lid tick.
        if display.is_asleep(primary) != Some(true) {
            if let Some(cur) = display.get_brightness(primary) {
                if cur > 0.01 {
                    let rounded = ((cur as f64) * 1000.0).round() / 1000.0;
                    if (rounded - next.saved_brightness).abs() >= OPEN_SYNC_EPSILON {
                        next.saved_brightness = rounded;
                        next.user_target = false;
                    }
                }
            }
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
pub fn run_guard_tick_locked(
    data_dir: &Path,
    probe: &dyn ProbeBackend,
    display: &dyn DisplayBackend,
) -> Result<GuardTickOutcome, String> {
    let settings = read_settings(data_dir);
    if !settings.guard_enabled {
        return Ok(GuardTickOutcome {
            paused: true,
            changed: false,
            fast_poll: false,
            open_restoring: false,
        });
    }
    let Ok(Some(_lock)) = FileLock::acquire(data_dir, false) else {
        return Ok(GuardTickOutcome {
            paused: false,
            changed: false,
            fast_poll: false,
            open_restoring: false,
        });
    };
    let closed = probe.lid_closed_fast();
    let pressure = probe.thermal_pressure_fast();
    let state = load_guard_state(data_dir);
    let now = now_epoch_secs();
    let (next, changed) = evaluate_guard_tick_with_thermal(
        closed,
        &pressure,
        settings.thermal_bright_cap_pct,
        &state,
        now,
        display,
    );
    if changed {
        save_guard_state(data_dir, &next)
            .map_err(|e| format!("failed to persist display-guard state: {e}"))?;
    }
    Ok(GuardTickOutcome {
        paused: false,
        changed,
        fast_poll: guard_wants_fast_poll(&next, now),
        open_restoring: guard_is_open_restoring(&next, now),
    })
}

pub fn run_watch_once(data_dir: &Path) -> Result<(), String> {
    let probe = MacOsProbeBackend;
    let display = MacOsDisplayBackend;
    let mut outcome = run_guard_tick_locked(data_dir, &probe, &display)?;
    // When invoked on lid open (e.g. via LaunchAgent), follow through display wake and the
    // short post-restore clobber watch within the same run instead of waiting another 10s.
    for _ in 0..12 {
        if !outcome.open_restoring {
            break;
        }
        std::thread::sleep(Duration::from_millis(FAST_POLL_INTERVAL_MS));
        outcome = run_guard_tick_locked(data_dir, &probe, &display)?;
    }
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
    let probe = MacOsProbeBackend;
    let display = MacOsDisplayBackend;
    let normal_dur = Duration::from_secs(interval_secs.clamp(2, 300));
    let fast_dur = Duration::from_millis(FAST_POLL_INTERVAL_MS);
    while !STOP_DAEMON.load(Ordering::Relaxed) {
        let fast = run_guard_tick_locked(data_dir, &probe, &display)
            .map(|o| o.fast_poll)
            .unwrap_or(false);
        let sleep_dur = if fast { fast_dur } else { normal_dur };
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
        drop_writes: Mutex<bool>,
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
                drop_writes: Mutex::new(false),
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
            if !*self.drop_writes.lock().unwrap() {
                *self.brightness.lock().unwrap() = Some(level);
            }
            true
        }
        fn sleep_display_now(&self) -> bool {
            *self.sleeps.lock().unwrap() += 1;
            *self.asleep.lock().unwrap() = true;
            true
        }
        fn lock_screen_now(&self) -> Result<&'static str, String> {
            Ok("mock")
        }
    }

    #[test]
    fn guard_fail_safe_open_sync_and_user_target_wake_restore() {
        let disp = MockDisplay::new(0.5, false);

        // 1. Fail-safe on unknown lid (ioreg timeout) skips CoreGraphics completely
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

        // 2. Steady-state open syncs changed brightness once and skips disk writes when steady
        let open_st = GuardState::default();
        let (st_sync, changed) = evaluate_guard_tick(Some(false), &open_st, 200.0, &disp);
        assert!(changed);
        assert!((st_sync.saved_brightness - 0.5).abs() < 1e-6);
        let (st_steady, changed) = evaluate_guard_tick(Some(false), &st_sync, 205.0, &disp);
        assert!(!changed);
        assert_eq!(st_steady, st_sync);

        // 3. Open + awake user brightness sets immediately & updates saved_brightness
        let (st_u1, changed, act1) =
            apply_user_brightness(65.0, Some(false), &st_steady, &disp).unwrap();
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
        assert!(st_c1.needs_restore);
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
    fn lid_open_waits_for_awake_restores_past_low_wake_and_guards_clobber() {
        let disp = MockDisplay::new(0.78, false);
        let open_st = GuardState::default();

        // 1. Open -> Closed saves 0.78, dims to 0%, sleeps display
        let (st_closed, changed) = evaluate_guard_tick(Some(true), &open_st, 100.0, &disp);
        assert!(changed);
        assert_eq!(st_closed.saved_brightness, 0.78);
        assert!(st_closed.needs_restore);
        assert!(*disp.asleep.lock().unwrap());
        assert_eq!(*disp.brightness.lock().unwrap(), Some(0.0));
        assert!(guard_wants_fast_poll(&st_closed, 100.0));

        // 2. Lid opens while panel is STILL asleep for 5 ticks -> does NOT write or burn retries
        disp.writes.lock().unwrap().clear();
        let mut st = st_closed;
        for i in 1..=5 {
            let (next, _) = evaluate_guard_tick(Some(false), &st, 110.0 + i as f64, &disp);
            assert_eq!(next.last_lid, "open");
            assert!(next.needs_restore);
            assert_eq!(next.restore_attempts, 0);
            assert!(disp.writes.lock().unwrap().is_empty());
            assert!(guard_is_open_restoring(&next, 110.0 + i as f64));
            st = next;
        }

        // 3. Panel wakes at dim 6% (Auto-Brightness or single F2 tap), AND WindowServer silently
        // drops the first write -> read-back catches it and retries without skipping via cur >= 0.02!
        *disp.asleep.lock().unwrap() = false;
        *disp.brightness.lock().unwrap() = Some(0.06);
        *disp.drop_writes.lock().unwrap() = true;
        let (st_drop, _) = evaluate_guard_tick(Some(false), &st, 116.0, &disp);
        assert!(st_drop.needs_restore);
        assert_eq!(st_drop.restore_attempts, 1);

        // 4. Next tick write sticks -> restores full 0.78 and arms post-restore clobber watch
        *disp.drop_writes.lock().unwrap() = false;
        let (st_ok, changed) = evaluate_guard_tick(Some(false), &st_drop, 116.5, &disp);
        assert!(changed);
        assert!(!st_ok.needs_restore);
        assert_eq!(st_ok.last_restore_ts, 116.5);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.78).abs() < 0.005);
        assert!(guard_is_open_restoring(&st_ok, 118.0));

        // 5. macOS loginwindow/CoreBrightness clobbers brightness back to 0% 2s later -> re-applied!
        *disp.brightness.lock().unwrap() = Some(0.0);
        let (st_guard, _) = evaluate_guard_tick(Some(false), &st_ok, 118.5, &disp);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.78).abs() < 0.005);
        assert_eq!(st_guard.saved_brightness, 0.78);

        // 6. User manually adjusts brightness to 50% during watch window -> NOT overridden!
        *disp.brightness.lock().unwrap() = Some(0.50);
        let (st_manual, _) = evaluate_guard_tick(Some(false), &st_guard, 120.0, &disp);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.50).abs() < 0.005);

        // 7. After watch window expires (>6s), open tracking syncs the user's new 50% level
        assert!(!guard_wants_fast_poll(&st_manual, 124.0));
        let (st_synced, changed) = evaluate_guard_tick(Some(false), &st_manual, 124.0, &disp);
        assert!(changed);
        assert!((st_synced.saved_brightness - 0.50).abs() < 0.005);

        // 8. If macOS already slept/zeroed display BEFORE our closed tick, saved 0.50 is preserved
        // and still restored when the screen wakes black, OR macOS's own visible wake is preserved.
        *disp.asleep.lock().unwrap() = true;
        *disp.brightness.lock().unwrap() = Some(0.0);
        let (st_pre_slept, _) = evaluate_guard_tick(Some(true), &st_synced, 140.0, &disp);
        assert!((st_pre_slept.saved_brightness - 0.50).abs() < 0.005);
        assert!(!st_pre_slept.needs_restore);

        *disp.asleep.lock().unwrap() = false;
        *disp.brightness.lock().unwrap() = Some(0.0);
        let (st_black_wake, _) = evaluate_guard_tick(Some(false), &st_pre_slept, 150.0, &disp);
        assert!(!st_black_wake.needs_restore);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.50).abs() < 0.005);

        // 9. Explicit low user_target (1%) restores to 1% without arming post-restore watch
        // (must never get bumped up to 20% on subsequent ticks).
        let low_target_st = GuardState {
            last_lid: "closed".into(),
            saved_brightness: 0.01,
            needs_restore: true,
            user_target: true,
            ..GuardState::default()
        };
        *disp.brightness.lock().unwrap() = Some(0.0);
        let (st_low_ok, _) = evaluate_guard_tick(Some(false), &low_target_st, 200.0, &disp);
        assert!(!st_low_ok.needs_restore);
        assert_eq!(st_low_ok.last_restore_ts, 0.0);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.01).abs() < 0.002);
        let (_, _) = evaluate_guard_tick(Some(false), &st_low_ok, 200.5, &disp);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.01).abs() < 0.002);

        // 10. Dropped write at the 5% dark-room floor is caught by scaled read-back tolerance
        let dark_floor_st = GuardState {
            last_lid: "closed".into(),
            saved_brightness: 0.05,
            needs_restore: true,
            user_target: false,
            ..GuardState::default()
        };
        *disp.brightness.lock().unwrap() = Some(0.0);
        *disp.drop_writes.lock().unwrap() = true;
        let (st_floor_drop, _) = evaluate_guard_tick(Some(false), &dark_floor_st, 220.0, &disp);
        assert!(st_floor_drop.needs_restore);
        assert_eq!(st_floor_drop.restore_attempts, 1);
        *disp.drop_writes.lock().unwrap() = false;
        let (st_floor_ok, _) = evaluate_guard_tick(Some(false), &st_floor_drop, 220.5, &disp);
        assert!(!st_floor_ok.needs_restore);
        assert!((disp.brightness.lock().unwrap().unwrap() - 0.05).abs() < 0.005);
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
