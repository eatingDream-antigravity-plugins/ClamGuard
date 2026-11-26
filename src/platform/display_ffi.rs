use std::ffi::CString;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub trait DisplayBackend: Send + Sync {
    /// Returns `(builtin_display_ids, external_display_ids)`.
    fn get_displays(&self) -> (Vec<u32>, Vec<u32>);
    fn is_asleep(&self, display_id: u32) -> Option<bool>;
    fn get_brightness(&self, display_id: u32) -> Option<f32>;
    fn set_brightness(&self, display_id: u32, level: f32) -> bool;
    fn sleep_display_now(&self) -> bool;
    fn lock_screen_now(&self) -> Result<&'static str, String>;
}

pub fn clamp_percent(value: f64) -> Result<u8, String> {
    if !value.is_finite() {
        return Err("percent must be a finite number between 0 and 100".to_string());
    }
    Ok(value.round().clamp(0.0, 100.0) as u8)
}

pub fn level_to_percent(level: f32) -> Option<u8> {
    if !level.is_finite() {
        return None;
    }
    Some((level.clamp(0.0, 1.0) * 100.0).round() as u8)
}

pub fn level_f64_to_percent(level: f64) -> Option<u8> {
    if !level.is_finite() {
        return None;
    }
    Some((level.clamp(0.0, 1.0) * 100.0).round() as u8)
}

#[derive(Clone, Debug)]
struct MockDisplayState {
    mode: String,
    level: f32,
    asleep: bool,
    locked: bool,
}

fn mock_display_mode() -> Option<String> {
    if let Ok(m) = std::env::var("MAC_CLAMSHELL_MOCK_DISPLAY") {
        let t = m.trim();
        if !t.is_empty() {
            return Some(t.to_ascii_lowercase());
        }
    }
    let has_mock_bright = std::env::var("MAC_CLAMSHELL_MOCK_BRIGHTNESS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some();
    let has_mock_lid = std::env::var("MAC_CLAMSHELL_MOCK_LID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some();
    if has_mock_bright || has_mock_lid {
        return Some("builtin".to_string());
    }
    None
}

fn mock_state_cell() -> &'static Mutex<Option<MockDisplayState>> {
    static MOCK: OnceLock<Mutex<Option<MockDisplayState>>> = OnceLock::new();
    MOCK.get_or_init(|| Mutex::new(None))
}

fn with_mock_display<R>(f: impl FnOnce(&mut MockDisplayState) -> R) -> Option<R> {
    let mode = mock_display_mode()?;
    let mut guard = mock_state_cell().lock().unwrap_or_else(|e| e.into_inner());
    let needs_init = match guard.as_ref() {
        Some(st) => st.mode != mode,
        None => true,
    };
    if needs_init {
        let init_pct = std::env::var("MAC_CLAMSHELL_MOCK_BRIGHTNESS")
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .and_then(|v| clamp_percent(v).ok())
            .unwrap_or(60);
        let init_closed = std::env::var("MAC_CLAMSHELL_MOCK_LID")
            .ok()
            .map(|s| s.trim() == "closed")
            .unwrap_or(false);
        *guard = Some(MockDisplayState {
            mode,
            level: if init_closed {
                0.0
            } else {
                (init_pct as f32) / 100.0
            },
            asleep: init_closed,
            locked: false,
        });
    }
    guard.as_mut().map(f)
}

#[cfg(target_os = "macos")]
mod darwin {
    use super::*;
    use std::ffi::c_void;

    type CgGetOnlineDisplayListFn =
        unsafe extern "C" fn(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    // CoreGraphics `boolean_t` is 32-bit unsigned int on macOS, not 1-byte C99 `_Bool`.
    type CgDisplayIsBuiltinFn = unsafe extern "C" fn(display: u32) -> u32;
    type CgDisplayIsAsleepFn = unsafe extern "C" fn(display: u32) -> u32;

    type DsGetBrightnessFn = unsafe extern "C" fn(display: u32, brightness: *mut f32) -> i32;
    type DsSetBrightnessFn = unsafe extern "C" fn(display: u32, brightness: f32) -> i32;
    type SacLockScreenImmediateFn = unsafe extern "C" fn();

    struct CgApi {
        get_online: CgGetOnlineDisplayListFn,
        is_builtin: CgDisplayIsBuiltinFn,
        is_asleep: CgDisplayIsAsleepFn,
    }

    struct DsApi {
        get_brightness: DsGetBrightnessFn,
        set_brightness: DsSetBrightnessFn,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGSessionCopyCurrentDictionary() -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            value: *const libc::c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(value: *const c_void) -> u8;
        fn CFRelease(value: *const c_void);
    }

    pub fn session_is_locked() -> Option<bool> {
        // The dictionary and key are owned references. The value is borrowed from the dictionary.
        unsafe {
            let dict = CGSessionCopyCurrentDictionary();
            if dict.is_null() {
                return None;
            }
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                c"CGSSessionScreenIsLocked".as_ptr(),
                0x0800_0100, // kCFStringEncodingUTF8
            );
            let locked = if key.is_null() {
                None
            } else {
                let val = CFDictionaryGetValue(dict, key);
                CFRelease(key);
                (!val.is_null() && CFGetTypeID(val) == CFBooleanGetTypeID())
                    .then(|| CFBooleanGetValue(val) != 0)
            };
            CFRelease(dict);
            locked
        }
    }

    unsafe fn load_sym(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        let ptr = libc::dlsym(handle, c.as_ptr());
        if ptr.is_null() {
            None
        } else {
            Some(ptr)
        }
    }

    fn cg_api() -> Option<&'static CgApi> {
        static CG: OnceLock<Option<CgApi>> = OnceLock::new();
        CG.get_or_init(|| unsafe {
            let path =
                CString::new("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
                    .ok()?;
            let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if handle.is_null() {
                return None;
            }
            let get_online = std::mem::transmute::<*mut c_void, CgGetOnlineDisplayListFn>(
                load_sym(handle, "CGGetOnlineDisplayList")?,
            );
            let is_builtin = std::mem::transmute::<*mut c_void, CgDisplayIsBuiltinFn>(load_sym(
                handle,
                "CGDisplayIsBuiltin",
            )?);
            let is_asleep = std::mem::transmute::<*mut c_void, CgDisplayIsAsleepFn>(load_sym(
                handle,
                "CGDisplayIsAsleep",
            )?);
            Some(CgApi {
                get_online,
                is_builtin,
                is_asleep,
            })
        })
        .as_ref()
    }

    fn ds_api() -> Option<&'static DsApi> {
        static DS: OnceLock<Option<DsApi>> = OnceLock::new();
        DS.get_or_init(|| unsafe {
            let path = CString::new(
                "/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices",
            )
            .ok()?;
            let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if handle.is_null() {
                return None;
            }
            let get_brightness = std::mem::transmute::<*mut c_void, DsGetBrightnessFn>(load_sym(
                handle,
                "DisplayServicesGetBrightness",
            )?);
            let set_brightness = std::mem::transmute::<*mut c_void, DsSetBrightnessFn>(load_sym(
                handle,
                "DisplayServicesSetBrightness",
            )?);
            Some(DsApi {
                get_brightness,
                set_brightness,
            })
        })
        .as_ref()
    }

    pub fn get_displays() -> (Vec<u32>, Vec<u32>) {
        let Some(api) = cg_api() else {
            return (Vec::new(), Vec::new());
        };
        let mut buf = [0u32; 16];
        let mut count = 0u32;
        let rc = unsafe { (api.get_online)(buf.len() as u32, buf.as_mut_ptr(), &mut count) };
        if rc != 0 {
            return (Vec::new(), Vec::new());
        }
        let n = (count as usize).min(buf.len());
        let mut builtins = Vec::new();
        let mut externals = Vec::new();
        for &did in &buf[..n] {
            if unsafe { (api.is_builtin)(did) != 0 } {
                builtins.push(did);
            } else {
                externals.push(did);
            }
        }
        (builtins, externals)
    }

    pub fn is_asleep(display_id: u32) -> Option<bool> {
        let api = cg_api()?;
        Some(unsafe { (api.is_asleep)(display_id) != 0 })
    }

    pub fn get_brightness(display_id: u32) -> Option<f32> {
        let api = ds_api()?;
        let mut val = 0.0f32;
        let rc = unsafe { (api.get_brightness)(display_id, &mut val) };
        if rc == 0 && val.is_finite() {
            Some(val)
        } else {
            None
        }
    }

    fn sac_lock_fn() -> Option<SacLockScreenImmediateFn> {
        static SAC: OnceLock<Option<SacLockScreenImmediateFn>> = OnceLock::new();
        *SAC.get_or_init(|| unsafe {
            let path = CString::new(
                "/System/Library/PrivateFrameworks/login.framework/Versions/Current/login",
            )
            .ok()?;
            let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if handle.is_null() {
                return None;
            }
            let sym = load_sym(handle, "SACLockScreenImmediate")?;
            Some(std::mem::transmute::<*mut c_void, SacLockScreenImmediateFn>(sym))
        })
    }

    pub fn set_brightness(display_id: u32, level: f32) -> bool {
        let Some(api) = ds_api() else {
            return false;
        };
        let clamped = level.clamp(0.0, 1.0);
        unsafe { (api.set_brightness)(display_id, clamped) == 0 }
    }

    pub fn lock_screen_immediate() -> bool {
        let Some(lock_fn) = sac_lock_fn() else {
            return false;
        };
        unsafe {
            lock_fn();
        }
        true
    }
}

fn confirm_display_sleep_lock(
    mut session_is_locked: impl FnMut() -> Option<bool>,
    timeout: Duration,
) -> Result<&'static str, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if session_is_locked() == Some(true) {
            return Ok("pmset");
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("pmset put the display to sleep without a verified macOS session lock; a password-after-sleep delay may be configured".to_string());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(40)));
    }
}

#[cfg(target_os = "macos")]
fn run_cmd_status(cmd: &str, args: &[&str], timeout_ms: u64) -> bool {
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => return false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacOsDisplayBackend;

impl DisplayBackend for MacOsDisplayBackend {
    fn get_displays(&self) -> (Vec<u32>, Vec<u32>) {
        if let Some(res) = with_mock_display(|st| match st.mode.as_str() {
            "none" | "desktop" | "docked" => (Vec::new(), vec![2]),
            _ => (vec![1], Vec::new()),
        }) {
            return res;
        }
        #[cfg(target_os = "macos")]
        {
            darwin::get_displays()
        }
        #[cfg(not(target_os = "macos"))]
        {
            (Vec::new(), Vec::new())
        }
    }

    fn is_asleep(&self, display_id: u32) -> Option<bool> {
        if let Some(res) = with_mock_display(|st| match st.mode.as_str() {
            "none" | "desktop" | "docked" | "unknown_sleep" => None,
            "asleep" => Some(true),
            _ => Some(st.asleep),
        }) {
            return res;
        }
        #[cfg(target_os = "macos")]
        {
            darwin::is_asleep(display_id)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = display_id;
            None
        }
    }

    fn get_brightness(&self, display_id: u32) -> Option<f32> {
        if let Some(res) = with_mock_display(|st| match st.mode.as_str() {
            "none" | "desktop" | "docked" => None,
            _ => Some(st.level),
        }) {
            return res;
        }
        #[cfg(target_os = "macos")]
        {
            darwin::get_brightness(display_id)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = display_id;
            None
        }
    }

    fn set_brightness(&self, display_id: u32, level: f32) -> bool {
        if let Some(res) = with_mock_display(|st| match st.mode.as_str() {
            "none" | "desktop" | "docked" | "fail_set" => false,
            _ => {
                st.level = level.clamp(0.0, 1.0);
                if st.level > 0.0 {
                    st.asleep = false;
                }
                true
            }
        }) {
            return res;
        }
        #[cfg(target_os = "macos")]
        {
            darwin::set_brightness(display_id, level)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (display_id, level);
            false
        }
    }

    fn sleep_display_now(&self) -> bool {
        if let Some(res) = with_mock_display(|st| {
            st.asleep = true;
            true
        }) {
            return res;
        }
        #[cfg(target_os = "macos")]
        {
            run_cmd_status("/usr/bin/pmset", &["displaysleepnow"], 3000)
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn lock_screen_now(&self) -> Result<&'static str, String> {
        if let Some(res) = with_mock_display(|st| match st.mode.as_str() {
            "fail_lock" => Err("mock lock screen failed".to_string()),
            "pmset_locked" | "pmset_unconfirmed" => {
                st.asleep = true;
                st.locked = st.mode == "pmset_locked";
                confirm_display_sleep_lock(|| Some(st.locked), Duration::ZERO)
            }
            _ => {
                st.locked = true;
                Ok("mock")
            }
        }) {
            return res;
        }
        #[cfg(target_os = "macos")]
        {
            if darwin::lock_screen_immediate() {
                return Ok("SACLockScreenImmediate");
            }
            if run_cmd_status(
                "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession",
                &["-suspend"],
                3000,
            ) {
                return Ok("CGSession");
            }
            if run_cmd_status("/usr/bin/pmset", &["displaysleepnow"], 3000) {
                return confirm_display_sleep_lock(
                    darwin::session_is_locked,
                    Duration::from_millis(800),
                );
            }
            Err("failed to lock screen via SACLockScreenImmediate, CGSession, or pmset".to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("lock screen is only supported on macOS".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_sleep_requires_confirmed_macos_session_lock() {
        let mut states = [None, Some(false), Some(true)].into_iter();
        assert_eq!(
            confirm_display_sleep_lock(|| states.next().unwrap(), Duration::from_millis(200)),
            Ok("pmset")
        );
        for state in [None, Some(false)] {
            let err = confirm_display_sleep_lock(|| state, Duration::ZERO).unwrap_err();
            assert!(err.contains("display to sleep without a verified macOS session lock"));
        }
    }

    fn mock_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn clamp_percent_and_mock_lock_screen() {
        assert_eq!(clamp_percent(-10.0).unwrap(), 0);
        assert_eq!(clamp_percent(0.0).unwrap(), 0);
        assert_eq!(clamp_percent(49.6).unwrap(), 50);
        assert_eq!(clamp_percent(150.0).unwrap(), 100);
        assert!(clamp_percent(f64::NAN).is_err());
        assert!(clamp_percent(f64::INFINITY).is_err());

        let _guard = mock_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prev_disp = std::env::var("MAC_CLAMSHELL_MOCK_DISPLAY").ok();

        std::env::set_var("MAC_CLAMSHELL_MOCK_DISPLAY", "builtin");
        {
            let mut st = mock_state_cell().lock().unwrap_or_else(|e| e.into_inner());
            *st = None;
        }

        let backend = MacOsDisplayBackend;
        let method = backend.lock_screen_now().expect("mock lock should succeed");
        assert_eq!(method, "mock");
        let is_locked = with_mock_display(|st| st.locked).unwrap_or(false);
        assert!(is_locked, "expected MockDisplayState.locked to be true");

        std::env::set_var("MAC_CLAMSHELL_MOCK_DISPLAY", "fail_lock");
        let err = backend.lock_screen_now().unwrap_err();
        assert!(err.contains("mock lock screen failed"));

        match prev_disp {
            Some(v) => std::env::set_var("MAC_CLAMSHELL_MOCK_DISPLAY", v),
            None => std::env::remove_var("MAC_CLAMSHELL_MOCK_DISPLAY"),
        }
        {
            let mut st = mock_state_cell().lock().unwrap_or_else(|e| e.into_inner());
            *st = None;
        }
    }
}
