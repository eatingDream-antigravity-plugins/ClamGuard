use std::ffi::CString;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub trait DisplayBackend: Send + Sync {
    /// Returns `(builtin_display_ids, external_display_ids)`.
    fn get_displays(&self) -> (Vec<u32>, Vec<u32>);
    fn is_asleep(&self, display_id: u32) -> Option<bool>;
    fn get_brightness(&self, display_id: u32) -> Option<f32>;
    fn set_brightness(&self, display_id: u32, level: f32) -> bool;
    fn sleep_display_now(&self) -> bool;
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

    struct CgApi {
        get_online: CgGetOnlineDisplayListFn,
        is_builtin: CgDisplayIsBuiltinFn,
        is_asleep: CgDisplayIsAsleepFn,
    }

    struct DsApi {
        get_brightness: DsGetBrightnessFn,
        set_brightness: DsSetBrightnessFn,
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
            let path = CString::new(
                "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics",
            )
            .ok()?;
            let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if handle.is_null() {
                return None;
            }
            let get_online = std::mem::transmute::<*mut c_void, CgGetOnlineDisplayListFn>(
                load_sym(handle, "CGGetOnlineDisplayList")?,
            );
            let is_builtin = std::mem::transmute::<*mut c_void, CgDisplayIsBuiltinFn>(
                load_sym(handle, "CGDisplayIsBuiltin")?,
            );
            let is_asleep = std::mem::transmute::<*mut c_void, CgDisplayIsAsleepFn>(
                load_sym(handle, "CGDisplayIsAsleep")?,
            );
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
            let get_brightness = std::mem::transmute::<*mut c_void, DsGetBrightnessFn>(
                load_sym(handle, "DisplayServicesGetBrightness")?,
            );
            let set_brightness = std::mem::transmute::<*mut c_void, DsSetBrightnessFn>(
                load_sym(handle, "DisplayServicesSetBrightness")?,
            );
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

    pub fn set_brightness(display_id: u32, level: f32) -> bool {
        let Some(api) = ds_api() else {
            return false;
        };
        let clamped = level.clamp(0.0, 1.0);
        unsafe { (api.set_brightness)(display_id, clamped) == 0 }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacOsDisplayBackend;

impl DisplayBackend for MacOsDisplayBackend {
    fn get_displays(&self) -> (Vec<u32>, Vec<u32>) {
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
        #[cfg(target_os = "macos")]
        {
            let Ok(mut child) = Command::new("/usr/bin/pmset")
                .arg("displaysleepnow")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                return false;
            };
            let deadline = Instant::now() + Duration::from_secs(3);
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
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_percent_validates_and_clamps() {
        assert_eq!(clamp_percent(-10.0).unwrap(), 0);
        assert_eq!(clamp_percent(0.0).unwrap(), 0);
        assert_eq!(clamp_percent(49.6).unwrap(), 50);
        assert_eq!(clamp_percent(150.0).unwrap(), 100);
        assert!(clamp_percent(f64::NAN).is_err());
        assert!(clamp_percent(f64::INFINITY).is_err());
    }
}
