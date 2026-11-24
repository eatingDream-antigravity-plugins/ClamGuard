use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
    DirBuilder::new().recursive(true).mode(0o700).create(dir)
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
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub mode_override: Option<String>,
    pub max_jobs_override: Option<i64>,
    pub ac_closed_profile: Option<String>,
    pub guard_enabled: Option<bool>,
    pub thermal_bright_cap_pct: Option<i64>,
    pub lang: Option<String>,
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
    let mut next = read_settings(data_dir);
    if let Some(m) = patch.mode_override.as_deref() {
        if VALID_MODES.contains(&m) {
            next.mode_override = m.to_string();
        }
    }
    if let Some(j) = patch.max_jobs_override {
        if (0..=64).contains(&j) {
            next.max_jobs_override = j as u32;
        }
    }
    if let Some(a) = patch.ac_closed_profile.as_deref() {
        if VALID_AC_PROFILES.contains(&a) {
            next.ac_closed_profile = a.to_string();
        }
    }
    if let Some(g) = patch.guard_enabled {
        next.guard_enabled = g;
    }
    if let Some(cap) = patch.thermal_bright_cap_pct {
        if (0..=100).contains(&cap) {
            next.thermal_bright_cap_pct = cap as u8;
        }
    }
    if let Some(ref raw_lang) = patch.lang {
        let trimmed = raw_lang.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
            next.lang = None;
        } else if matches!(trimmed, "en" | "zh-CN" | "zh" | "zh_CN") {
            next.lang = crate::i18n::normalize_setting_lang(trimmed);
        }
    }
    let body = serde_json::to_string_pretty(&next)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(&settings_path(data_dir), format!("{body}\n").as_bytes())?;
    Ok(next)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardState {
    #[serde(default = "default_last_lid")]
    pub last_lid: String,
    #[serde(default = "default_saved_brightness")]
    pub saved_brightness: f64,
    #[serde(default)]
    pub last_sleep_ts: f64,
    #[serde(default)]
    pub needs_restore: bool,
    #[serde(default)]
    pub restore_attempts: u32,
    #[serde(default)]
    pub user_target: bool,
    #[serde(default)]
    pub thermal_capped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_thermal_brightness: Option<f64>,
}

fn default_last_lid() -> String {
    "open".to_string()
}

fn default_saved_brightness() -> f64 {
    0.20
}

impl Default for GuardState {
    fn default() -> Self {
        Self {
            last_lid: default_last_lid(),
            saved_brightness: default_saved_brightness(),
            last_sleep_ts: 0.0,
            needs_restore: false,
            restore_attempts: 0,
            user_target: false,
            thermal_capped: false,
            pre_thermal_brightness: None,
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
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("state");
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
        ensure_private_dir(data_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path(data_dir))?;
        let flags = if blocking {
            libc::LOCK_EX
        } else {
            libc::LOCK_EX | libc::LOCK_NB
        };
        let rc = unsafe { libc::flock(file.as_raw_fd(), flags) };
        if rc == 0 {
            Ok(Some(Self { file }))
        } else {
            let err = io::Error::last_os_error();
            if !blocking && err.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(err)
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
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(((data.len() + 9 + 63) / 64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in padded.chunks_exact(64) {
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

        let mode = fs::metadata(settings_path(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // Invalid values are ignored safely
        let s2 = write_settings(
            &dir,
            &SettingsPatch {
                mode_override: Some("bogus".into()),
                max_jobs_override: Some(999),
                ac_closed_profile: Some("bogus".into()),
                guard_enabled: None,
                thermal_bright_cap_pct: Some(250),
                lang: Some("bogus".into()),
            },
        )
        .unwrap();
        assert_eq!(s2, s);

        let s3 = write_settings(
            &dir,
            &SettingsPatch {
                lang: Some("auto".into()),
                ..SettingsPatch::default()
            },
        )
        .unwrap();
        assert_eq!(s3.lang, None);
        let _ = fs::remove_dir_all(&dir);
    }
}
