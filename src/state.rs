use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

pub const VALID_MODES: &[&str] = &["auto", "eco", "utility", "strict", "normal"];
pub const VALID_AC_PROFILES: &[&str] = &["utility", "eco"];

pub fn default_data_dir() -> PathBuf {
    if let Ok(v) = std::env::var("MAC_CLAMSHELL_DATA_DIR") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Ok(v) = std::env::var("PLUGIN_DATA") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home)
                .join(".gemini")
                .join("antigravity")
                .join("plugin_data")
                .join("mac-clamshell-mcp");
        }
    }
    std::env::temp_dir().join("mac-clamshell-mcp")
}

pub fn abbreviate_home(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Ok(rel) = path.strip_prefix(&home) {
                let rel_s = rel.display().to_string();
                return if rel_s.is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{rel_s}")
                };
            }
        }
    }
    path.display().to_string()
}

pub fn resolve_data_dir(cli_override: Option<&Path>) -> PathBuf {
    match cli_override {
        Some(p) if !p.as_os_str().to_string_lossy().trim().is_empty() => p.to_path_buf(),
        _ => default_data_dir(),
    }
}

pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing symlink directory: {}", dir.display()),
                ));
            }
            if !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("path is not a directory: {}", dir.display()),
                ));
            }
            if (meta.permissions().mode() & 0o777) != 0o700 {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

pub fn guard_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("display-guard-state.json")
}

pub fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("display-guard.lock")
}

pub fn daemon_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("display-guard-daemon.lock")
}

pub type FileFingerprint = Option<(u128, u64)>;

pub fn file_fingerprint(path: &Path) -> FileFingerprint {
    let meta = fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((mtime_ns, meta.len()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "default_mode_override")]
    pub mode_override: String,
    #[serde(default)]
    pub max_jobs_override: u32,
    #[serde(default = "default_ac_closed_profile")]
    pub ac_closed_profile: String,
    #[serde(default = "default_guard_enabled")]
    pub guard_enabled: bool,
    #[serde(default)]
    pub thermal_bright_cap_pct: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

fn default_mode_override() -> String {
    "auto".to_string()
}

fn default_ac_closed_profile() -> String {
    "utility".to_string()
}

fn default_guard_enabled() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode_override: default_mode_override(),
            max_jobs_override: 0,
            ac_closed_profile: default_ac_closed_profile(),
            guard_enabled: true,
            thermal_bright_cap_pct: 0,
            lang: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub mode_override: Option<String>,
    pub max_jobs_override: Option<i64>,
    pub ac_closed_profile: Option<String>,
    pub guard_enabled: Option<bool>,
    pub thermal_bright_cap_pct: Option<i64>,
    pub lang: Option<String>,
}

impl SettingsPatch {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(m) = self.mode_override.as_deref() {
            if !VALID_MODES.contains(&m) {
                return Err(format!(
                    "invalid modeOverride {:?}; expected one of: {}",
                    m,
                    VALID_MODES.join(", ")
                ));
            }
        }
        if let Some(j) = self.max_jobs_override {
            if !(0..=64).contains(&j) {
                return Err(format!(
                    "invalid maxJobsOverride {j}; expected integer in 0..=64"
                ));
            }
        }
        if let Some(a) = self.ac_closed_profile.as_deref() {
            if !VALID_AC_PROFILES.contains(&a) {
                return Err(format!(
                    "invalid acClosedProfile {:?}; expected one of: {}",
                    a,
                    VALID_AC_PROFILES.join(", ")
                ));
            }
        }
        if let Some(cap) = self.thermal_bright_cap_pct {
            if !(0..=100).contains(&cap) {
                return Err(format!(
                    "invalid thermalBrightCapPct {cap}; expected integer in 0..=100"
                ));
            }
        }
        if let Some(ref raw_lang) = self.lang {
            let trimmed = raw_lang.trim();
            if !(trimmed.is_empty()
                || trimmed.eq_ignore_ascii_case("auto")
                || matches!(trimmed, "en" | "zh-CN" | "zh" | "zh_CN"))
            {
                return Err(format!(
                    "invalid lang {:?}; expected one of: auto, en, zh-CN",
                    raw_lang
                ));
            }
        }
        Ok(())
    }
}

pub fn read_settings(data_dir: &Path) -> Settings {
    let path = settings_path(data_dir);
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Settings::default(),
    };
    let val: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Settings::default(),
    };
    let mode_override = val
        .get("modeOverride")
        .and_then(|v| v.as_str())
        .filter(|s| VALID_MODES.contains(s))
        .unwrap_or("auto")
        .to_string();
    let max_jobs_override = val
        .get("maxJobsOverride")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
        .unwrap_or(0);
    let ac_closed_profile = val
        .get("acClosedProfile")
        .and_then(|v| v.as_str())
        .filter(|s| VALID_AC_PROFILES.contains(s))
        .unwrap_or("utility")
        .to_string();
    let guard_enabled = val
        .get("guardEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let thermal_bright_cap_pct = val
        .get("thermalBrightCapPct")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(100) as u8)
        .unwrap_or(0);
    let lang = val
        .get("lang")
        .and_then(|v| v.as_str())
        .and_then(crate::i18n::normalize_setting_lang);
    Settings {
        mode_override,
        max_jobs_override,
        ac_closed_profile,
        guard_enabled,
        thermal_bright_cap_pct,
        lang,
    }
}

pub fn write_settings(data_dir: &Path, patch: &SettingsPatch) -> io::Result<Settings> {
    patch
        .validate()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let _lock = FileLock::acquire_path(&data_dir.join("settings.lock"), true)?;
    let mut next = read_settings(data_dir);
    if let Some(m) = patch.mode_override.as_deref() {
        next.mode_override = m.to_string();
    }
    if let Some(j) = patch.max_jobs_override {
        next.max_jobs_override = j as u32;
    }
    if let Some(a) = patch.ac_closed_profile.as_deref() {
        next.ac_closed_profile = a.to_string();
    }
    if let Some(g) = patch.guard_enabled {
        next.guard_enabled = g;
    }
    if let Some(cap) = patch.thermal_bright_cap_pct {
        next.thermal_bright_cap_pct = cap as u8;
    }
    if let Some(ref raw_lang) = patch.lang {
        let trimmed = raw_lang.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
            next.lang = None;
        } else {
            next.lang = crate::i18n::normalize_setting_lang(trimmed);
        }
    }
    let body = serde_json::to_string_pretty(&next)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(&settings_path(data_dir), format!("{body}\n").as_bytes())?;
    Ok(next)
}

/// Explicit user brightness intent (owned by user brightness controls / DP5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserBrightnessIntent {
    #[serde(default = "default_saved_brightness")]
    pub saved_brightness: f64,
    #[serde(default)]
    pub user_target: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub user_override_thermal: bool,
}

impl Default for UserBrightnessIntent {
    fn default() -> Self {
        Self {
            saved_brightness: default_saved_brightness(),
            user_target: false,
            user_override_thermal: false,
        }
    }
}

/// Internal Display Guard state machine (owned by guard daemon / DP4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardMachineState {
    #[serde(default = "default_last_lid")]
    pub last_lid: String,
    #[serde(default)]
    pub last_sleep_ts: f64,
    #[serde(default)]
    pub needs_restore: bool,
    #[serde(default)]
    pub restore_attempts: u32,
    #[serde(default)]
    pub thermal_capped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_thermal_brightness: Option<f64>,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub last_restore_ts: f64,
}

impl Default for GuardMachineState {
    fn default() -> Self {
        Self {
            last_lid: default_last_lid(),
            last_sleep_ts: 0.0,
            needs_restore: false,
            restore_attempts: 0,
            thermal_capped: false,
            pre_thermal_brightness: None,
            last_restore_ts: 0.0,
        }
    }
}

/// Persisted display-guard-state.json combining user intent and guard machine state
/// with flat on-disk JSON compatibility.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GuardState {
    #[serde(flatten)]
    pub user_intent: UserBrightnessIntent,
    #[serde(flatten)]
    pub machine: GuardMachineState,
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn default_last_lid() -> String {
    "open".to_string()
}

fn default_saved_brightness() -> f64 {
    0.20
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayGuardSummary {
    pub last_lid: String,
    pub saved_brightness: Option<f64>,
    pub last_sleep_ts: f64,
    pub needs_restore: bool,
    pub user_target: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub user_override_thermal: bool,
    pub thermal_capped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_thermal_brightness: Option<f64>,
}

impl From<&GuardState> for DisplayGuardSummary {
    fn from(s: &GuardState) -> Self {
        let effective_thermal_cap =
            s.machine.thermal_capped && !s.user_intent.user_override_thermal;
        Self {
            last_lid: s.machine.last_lid.clone(),
            saved_brightness: if s.user_intent.saved_brightness.is_finite() {
                Some((s.user_intent.saved_brightness * 1000.0).round() / 1000.0)
            } else {
                None
            },
            last_sleep_ts: s.machine.last_sleep_ts,
            needs_restore: s.machine.needs_restore,
            user_target: s.user_intent.user_target,
            user_override_thermal: s.user_intent.user_override_thermal,
            thermal_capped: effective_thermal_cap,
            pre_thermal_brightness: if effective_thermal_cap {
                s.machine
                    .pre_thermal_brightness
                    .filter(|v| v.is_finite())
                    .map(|v| (v * 1000.0).round() / 1000.0)
            } else {
                None
            },
        }
    }
}

pub fn load_guard_state(data_dir: &Path) -> GuardState {
    let path = guard_state_path(data_dir);
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str::<GuardState>(&raw).unwrap_or_default(),
        Err(_) => GuardState::default(),
    }
}

pub fn load_guard_state_opt(data_dir: &Path) -> Option<GuardState> {
    let path = guard_state_path(data_dir);
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str::<GuardState>(&raw).ok()
}

pub fn save_guard_state(data_dir: &Path, state: &GuardState) -> io::Result<()> {
    let body = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(&guard_state_path(data_dir), format!("{body}\n").as_bytes())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("state");
    let tmp = path.with_file_name(format!("{fname}.tmp.{}.{seq}", std::process::id()));
    let write_res = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes))
        .and_then(|_| fs::rename(&tmp, path));
    if write_res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_res
}

pub struct FileLock {
    file: File,
}

impl FileLock {
    pub fn acquire(data_dir: &Path, blocking: bool) -> io::Result<Option<Self>> {
        Self::acquire_path(&lock_path(data_dir), blocking)
    }

    fn acquire_path(path: &Path, blocking: bool) -> io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            ensure_private_dir(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let flags = if blocking {
            libc::LOCK_EX
        } else {
            libc::LOCK_EX | libc::LOCK_NB
        };
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 {
                return Ok(Some(Self { file }));
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock if !blocking => return Ok(None),
                _ => return Err(err),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Exclusive non-blocking daemon ownership lock (`display-guard-daemon.lock`) so only one
/// long-lived guard loop (standalone `watch --daemon` or fallback `serve` observer) runs at a time.
pub struct DaemonLock {
    _lock: FileLock,
}

impl DaemonLock {
    pub fn try_acquire(data_dir: &Path) -> io::Result<Option<Self>> {
        let path = daemon_lock_path(data_dir);
        for attempt in 0..=3 {
            if let Some(lock) = FileLock::acquire_path(&path, false)? {
                return Ok(Some(Self { _lock: lock }));
            }
            if attempt < 3 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(None)
    }

    pub fn is_active(data_dir: &Path) -> bool {
        let path = daemon_lock_path(data_dir);
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        else {
            return false;
        };
        let fd = file.as_raw_fd();
        loop {
            if unsafe { libc::flock(fd, libc::LOCK_SH | libc::LOCK_NB) } == 0 {
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                }
                return false;
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return true,
                _ => return false,
            }
        }
    }
}

pub fn now_epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn now_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Format Unix epoch milliseconds as RFC 3339 / ISO-8601 UTC (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
pub fn format_iso8601_utc(epoch_ms: u128) -> String {
    let total_secs = (epoch_ms / 1000) as i64;
    let millis = (epoch_ms % 1000) as u32;
    let days = total_secs.div_euclid(86_400);
    let sod = total_secs.rem_euclid(86_400);
    let hour = (sod / 3600) as u32;
    let min = ((sod % 3600) / 60) as u32;
    let sec = (sod % 60) as u32;

    // Howard Hinnant's civil_from_days algorithm (exact proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

/// Pure FIPS 180-4 SHA-256 returning full 64 lowercase hex characters.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in padded.as_chunks::<64>().0 {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        use std::fmt::Write;
        let _ = write!(&mut out, "{word:08x}");
    }
    out
}

pub fn sha256_hex_16(data: &[u8]) -> String {
    sha256_hex(data)[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_settings_patches_preserve_disjoint_fields() {
        let dir = std::env::temp_dir().join(format!(
            "clamshell-settings-concurrent-{}",
            std::process::id()
        ));
        for round in 0..16 {
            let dir = dir.join(round.to_string());
            let patches = [
                SettingsPatch {
                    mode_override: Some("strict".into()),
                    ..SettingsPatch::default()
                },
                SettingsPatch {
                    guard_enabled: Some(false),
                    ..SettingsPatch::default()
                },
                SettingsPatch {
                    lang: Some("zh-CN".into()),
                    ..SettingsPatch::default()
                },
            ];
            let barrier = std::sync::Barrier::new(patches.len());
            std::thread::scope(|scope| {
                for patch in patches {
                    let (dir, barrier) = (&dir, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        write_settings(dir, &patch).unwrap();
                    });
                }
            });
            let settings = read_settings(&dir);
            assert_eq!(settings.mode_override, "strict");
            assert!(!settings.guard_enabled);
            assert_eq!(settings.lang.as_deref(), Some("zh-CN"));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sha256_and_iso8601_helpers() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex_16(b"abc"), "ba7816bf8f01cfea");
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_iso8601_utc(1_700_000_000_123),
            "2023-11-14T22:13:20.123Z"
        );
    }

    #[test]
    fn settings_roundtrip_and_validation() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("mac-clamshell-state-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(read_settings(&dir), Settings::default());
        assert_eq!(resolve_data_dir(Some(Path::new(""))), default_data_dir());

        let s = write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("eco".into()),
                max_jobs_override: Some(3),
                ac_closed_profile: Some("eco".into()),
                guard_enabled: Some(false),
                thermal_bright_cap_pct: Some(45),
                lang: Some("zh-CN".into()),
            },
        )
        .unwrap();
        assert_eq!(s.mode_override, "eco");
        assert_eq!(s.max_jobs_override, 3);
        assert_eq!(s.ac_closed_profile, "eco");
        assert!(!s.guard_enabled);
        assert_eq!(s.thermal_bright_cap_pct, 45);
        assert_eq!(s.lang.as_deref(), Some("zh-CN"));

        let mode = fs::metadata(settings_path(&dir))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        // Invalid values and unknown fields are rejected without mutating settings.json
        for bad_patch in [
            SettingsPatch {
                mode_override: Some("bogus".into()),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                max_jobs_override: Some(999),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                max_jobs_override: Some(-1),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                ac_closed_profile: Some("bogus".into()),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                thermal_bright_cap_pct: Some(250),
                ..SettingsPatch::default()
            },
            SettingsPatch {
                lang: Some("bogus".into()),
                ..SettingsPatch::default()
            },
        ] {
            let err = write_settings(&dir, &bad_patch).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"unknownField": 1}"#).is_err());
        assert_eq!(read_settings(&dir), s);

        // ensure_private_dir tightens existing 0o755 dirs to 0o700 and refuses symlinks
        let loose_dir = dir.join("loose");
        fs::create_dir(&loose_dir).unwrap();
        fs::set_permissions(&loose_dir, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&loose_dir).unwrap();
        assert_eq!(
            fs::metadata(&loose_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let sym_dir = dir.join("sym-dir");
        std::os::unix::fs::symlink(&loose_dir, &sym_dir).unwrap();
        assert_eq!(
            ensure_private_dir(&sym_dir).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let s3 = write_settings(
            &dir,
            &SettingsPatch {
                lang: Some("auto".into()),
                ..SettingsPatch::default()
            },
        )
        .unwrap();
        assert_eq!(s3.lang, None);

        // GuardState flat JSON compatibility across separated user_intent / machine structs
        let legacy = r#"{"last_lid":"closed","saved_brightness":0.72,"last_sleep_ts":10.0,"needs_restore":true,"restore_attempts":2,"user_target":true,"thermal_capped":true,"pre_thermal_brightness":0.85}"#;
        let parsed: GuardState = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.machine.last_lid, "closed");
        assert_eq!(parsed.user_intent.saved_brightness, 0.72);
        assert!(parsed.user_intent.user_target);
        assert!(!parsed.user_intent.user_override_thermal);
        assert!(parsed.machine.thermal_capped);
        assert_eq!(parsed.machine.pre_thermal_brightness, Some(0.85));
        let reserialized = serde_json::to_value(&parsed).unwrap();
        assert_eq!(reserialized["last_lid"], "closed");
        assert_eq!(reserialized["saved_brightness"], 0.72);

        // DaemonLock enforces exclusive guard daemon ownership and lock-free concurrent readers
        assert!(!DaemonLock::is_active(&dir));
        let dlock = DaemonLock::try_acquire(&dir).unwrap().unwrap();
        assert!(DaemonLock::is_active(&dir));
        assert!(DaemonLock::try_acquire(&dir).unwrap().is_none());
        drop(dlock);
        assert!(!DaemonLock::is_active(&dir));

        // Concurrent read-only LOCK_SH probes must never falsely report active when no daemon runs
        let readers = 12;
        let barrier = std::sync::Barrier::new(readers);
        std::thread::scope(|scope| {
            for _ in 0..readers {
                let (dir, barrier) = (&dir, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..64 {
                        assert!(
                            !DaemonLock::is_active(dir),
                            "concurrent LOCK_SH probes must never falsely report active"
                        );
                    }
                });
            }
        });

        let _ = fs::remove_dir_all(&dir);
    }
}
