use super::ProbeCache;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::io::Read;
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

pub const CMD_TIMEOUT_MS: u64 = 2500;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySummary {
    pub total_gb: f64,
    pub used_gb: f64,
    pub compressed_gb: f64,
    pub used_pct: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticHostInfo {
    pub hostname: String,
    pub chip: String,
    pub macos_version: String,
    pub cpu_topology: CpuTopology,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HardwareProbe {
    pub lid_state: String,
    pub clamshell_causes_sleep: String,
    pub power_source: String,
    pub battery_pct: Option<u32>,
    pub battery_detail: String,
    pub thermal_state: String,
    pub thermal_pressure: String,
    pub cpu_speed_limit_pct: Option<u32>,
    pub low_power_mode: String,
    pub memory: MemorySummary,
    pub load_avg: [f64; 3],
    pub uptime_seconds: u64,
    pub degraded: bool,
}

pub trait ProbeBackend: Send + Sync {
    fn static_host(&self) -> StaticHostInfo;
    fn probe_hardware(&self) -> HardwareProbe;
    fn probe_hardware_fresh(&self) -> HardwareProbe {
        self.probe_hardware()
    }
    /// Uncached lid probe for display write decisions.
    fn lid_closed_fast(&self) -> Option<bool>;
    fn thermal_pressure_fast(&self) -> String {
        "unknown".to_string()
    }
}

pub fn parse_bool_token(val: &str) -> Option<bool> {
    match val.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

pub fn map_darwin_thermal_level(raw: u64) -> &'static str {
    match raw {
        0 => "nominal",
        1 => "fair",
        2 => "serious",
        _ => "critical",
    }
}

/// Build accurate CPU topology: when `perflevel0 == 0` (Intel / homogeneous Mac),
/// report homogeneous cores (`performance_cores = 0`, `efficiency_cores = 0`).
pub fn build_cpu_topology(total: u32, perflevel0: u32, perflevel1: u32) -> CpuTopology {
    let total = total.max(1);
    if perflevel0 == 0 {
        CpuTopology {
            total,
            performance_cores: 0,
            efficiency_cores: 0,
        }
    } else {
        let e_cores = if perflevel1 > 0 {
            perflevel1
        } else {
            total.saturating_sub(perflevel0).max(1)
        };
        CpuTopology {
            total: total.max(perflevel0 + e_cores),
            performance_cores: perflevel0,
            efficiency_cores: e_cores,
        }
    }
}

pub fn run_cmd_timeout(cmd: &str, args: &[&str], timeout_ms: u64) -> (bool, String) {
    let (code, stdout) = run_cmd_exit_code(cmd, args, timeout_ms);
    (code == Some(0), stdout)
}

pub fn run_cmd_exit_code(cmd: &str, args: &[&str], timeout_ms: u64) -> (Option<i32>, String) {
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return (None, String::new());
    };

    let Some(mut stdout_pipe) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return (None, String::new());
    };

    // Drain stdout on a scoped reader thread so large output never deadlocks wait().
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let mut limited = stdout_pipe.by_ref().take(512 * 1024);
        let _ = limited.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut poll_ms = 1u64;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
                poll_ms = (poll_ms * 2).min(8);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    let stdout = reader.join().unwrap_or_default();
    (code, stdout)
}

pub fn parse_ioreg_clamshell(out: &str) -> (String, String) {
    let mut lid = "unknown".to_string();
    let mut causes_sleep = "unknown".to_string();
    for line in out.lines() {
        if lid == "unknown" && line.contains("\"AppleClamshellState\"") {
            if let Some((_, rhs)) = line.split_once('=') {
                if let Some(b) = parse_bool_token(rhs) {
                    lid = if b { "closed" } else { "open" }.to_string();
                }
            }
        }
        if causes_sleep == "unknown" && line.contains("\"AppleClamshellCausesSleep\"") {
            if let Some((_, rhs)) = line.split_once('=') {
                if let Some(b) = parse_bool_token(rhs) {
                    causes_sleep = if b { "yes" } else { "no" }.to_string();
                }
            }
        }
        if lid != "unknown" && causes_sleep != "unknown" {
            break;
        }
    }
    (lid, causes_sleep)
}

pub fn parse_pmset_batt(out: &str) -> (String, Option<u32>, String) {
    let power_source = if out.contains("Battery Power") {
        "battery"
    } else if out.contains("AC Power") {
        "ac"
    } else {
        "unknown"
    }
    .to_string();

    let mut pct = None;
    let bytes = out.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'%' && i > 0 {
            let mut start = i;
            while start > 0 && bytes[start - 1].is_ascii_digit() {
                start -= 1;
            }
            if start < i {
                if let Ok(n) = out[start..i].parse::<u32>() {
                    pct = Some(n);
                    break;
                }
            }
        }
    }

    let detail = out
        .lines()
        .nth(1)
        .map(|l| l.trim().to_string())
        .unwrap_or_default();

    (power_source, pct, detail)
}

/// Parse `pmset -g therm`. Returns `(thermal_state, cpu_speed_limit_pct)` where `cpu_speed_limit_pct`
/// is `Some(pct)` ONLY when Intel XCPM `CPU_Speed_Limit` was actually reported by macOS.
pub fn parse_pmset_therm(out: &str) -> (String, Option<u32>) {
    let mut thermal_state = "unknown";
    let mut speed_limit = None;
    let mut has_level = false;
    let mut no_warning = false;

    for line in out.lines() {
        let low = line.to_ascii_lowercase();
        if low.contains("no thermal warning") {
            no_warning = true;
        }
        if low.contains("thermal_level")
            || (low.contains("thermal warning level") && line.contains('='))
        {
            has_level = true;
        }
        if line.contains("CPU_Speed_Limit") {
            if let Some((_, rhs)) = line.split_once('=') {
                let digits: String = rhs.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = digits.parse::<u32>() {
                    speed_limit = Some(v);
                    if v < 90 {
                        thermal_state = "warn";
                    } else if thermal_state == "unknown" {
                        thermal_state = "ok";
                    }
                }
            }
        }
        if line.contains("CPU_Scheduler_Limit") {
            if let Some((_, rhs)) = line.split_once('=') {
                let digits: String = rhs.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = digits.parse::<u32>() {
                    if v < 90 {
                        thermal_state = "warn";
                    } else if thermal_state == "unknown" {
                        thermal_state = "ok";
                    }
                }
            }
        }
    }

    if has_level && !no_warning {
        thermal_state = "warn";
    } else if no_warning && thermal_state == "unknown" {
        thermal_state = "ok";
    }
    (thermal_state.to_string(), speed_limit)
}

pub fn resolve_thermal_status(
    mock_therm: Option<&str>,
    mock_pressure: Option<&str>,
    darwin_pressure: Option<&str>,
    pmset_out: &str,
) -> (String, String, Option<u32>) {
    if let Some(raw) = mock_pressure.or(mock_therm) {
        let trimmed = raw.trim().to_ascii_lowercase();
        if let Some(rest) = trimmed.strip_prefix("intel_warn:") {
            let spd = rest.parse::<u32>().ok().unwrap_or(72);
            return ("warn".to_string(), "serious".to_string(), Some(spd));
        }
        return match trimmed.as_str() {
            "nominal" | "ok" | "0" => ("ok".to_string(), "nominal".to_string(), None),
            "fair" | "moderate" | "1" => ("ok".to_string(), "fair".to_string(), None),
            "serious" | "heavy" | "warn" | "2" => ("warn".to_string(), "serious".to_string(), None),
            "critical" | "trapping" | "3" => ("warn".to_string(), "critical".to_string(), None),
            _ => ("unknown".to_string(), "unknown".to_string(), None),
        };
    }

    let (pm_state, speed_limit) = parse_pmset_therm(pmset_out);
    let pressure = darwin_pressure
        .unwrap_or(match pm_state.as_str() {
            "warn" => "serious",
            "ok" => "nominal",
            _ => "unknown",
        })
        .to_string();
    let state = if pm_state == "warn" || matches!(pressure.as_str(), "serious" | "critical") {
        "warn"
    } else if pressure == "unknown" {
        "unknown"
    } else {
        "ok"
    }
    .to_string();
    (state, pressure, speed_limit)
}

pub fn parse_low_power_mode(out: &str) -> String {
    for line in out.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("lowpowermode") {
            if let Some(val) = rest.split_whitespace().next() {
                if !val.is_empty() {
                    return val.to_string();
                }
            }
        }
    }
    "unknown".to_string()
}

#[cfg(target_os = "macos")]
fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(target_os = "macos")]
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;

    pub fn sysctl_string(name: &str) -> Option<String> {
        let cname = CString::new(name).ok()?;
        let mut size: libc::size_t = 0;
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size == 0 || size > 4096 {
            return None;
        }
        let mut buf = vec![0u8; size];
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(size.min(buf.len()));
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8(buf).ok()
    }

    pub fn sysctl_u64(name: &str) -> Option<u64> {
        let cname = CString::new(name).ok()?;
        let mut raw = [0u8; 8];
        let mut size = raw.len();
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                raw.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            if size == 8 {
                return Some(u64::from_ne_bytes(raw));
            }
            if size == 4 {
                return Some(u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64);
            }
        }
        // Fallback explicit 32-bit query for keys that reject 8-byte buffers.
        let mut v32: u32 = 0;
        let mut s32 = std::mem::size_of::<u32>();
        let rc32 = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                (&mut v32 as *mut u32) as *mut libc::c_void,
                &mut s32,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc32 == 0 && s32 == std::mem::size_of::<u32>() {
            Some(v32 as u64)
        } else {
            None
        }
    }

    pub fn hostname() -> String {
        let mut buf = [0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc != 0 {
            return "Mac".to_string();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    pub fn load_avg() -> [f64; 3] {
        let mut arr = [0.0f64; 3];
        let n = unsafe { libc::getloadavg(arr.as_mut_ptr(), 3) };
        if n == 3 {
            [round2(arr[0]), round2(arr[1]), round2(arr[2])]
        } else {
            [0.0, 0.0, 0.0]
        }
    }

    pub fn uptime_seconds() -> u64 {
        let Ok(cname) = CString::new("kern.boottime") else {
            return 0;
        };
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of::<libc::timeval>();
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                (&mut tv as *mut libc::timeval) as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || tv.tv_sec <= 0 {
            return 0;
        }
        let now_sec = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        (now_sec - tv.tv_sec).max(0) as u64
    }

    #[repr(C)]
    #[derive(Default)]
    struct VmStatistics64 {
        free_count: u32,
        active_count: u32,
        inactive_count: u32,
        wire_count: u32,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: u32,
        speculative_count: u32,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: u32,
        throttled_count: u32,
        external_page_count: u32,
        internal_page_count: u32,
        total_uncompressed_pages_in_compressor: u64,
    }

    extern "C" {
        fn mach_host_self() -> u32;
        fn host_page_size(host: u32, out_page_size: *mut usize) -> i32;
        fn host_statistics64(
            host_priv: u32,
            flavor: i32,
            host_info_out: *mut i32,
            host_info_out_cnt: *mut u32,
        ) -> i32;

        fn notify_register_check(name: *const libc::c_char, out_token: *mut libc::c_int) -> u32;
        fn notify_get_state(token: libc::c_int, state64: *mut u64) -> u32;
        fn notify_cancel(token: libc::c_int) -> u32;
    }

    pub fn thermal_pressure_level() -> Option<&'static str> {
        const KEY: &[u8] = b"com.apple.system.thermalpressurelevel\0";
        static TOKEN: Mutex<Option<libc::c_int>> = Mutex::new(None);

        let mut guard = TOKEN.lock().unwrap_or_else(|e| e.into_inner());
        for _attempt in 0..2 {
            let tok = match *guard {
                Some(t) => t,
                None => {
                    let mut out_tok: libc::c_int = -1;
                    let rc = unsafe {
                        notify_register_check(KEY.as_ptr() as *const libc::c_char, &mut out_tok)
                    };
                    if rc != 0 || out_tok < 0 {
                        return None;
                    }
                    *guard = Some(out_tok);
                    out_tok
                }
            };
            let mut state64: u64 = 0;
            let rc = unsafe { notify_get_state(tok, &mut state64) };
            if rc == 0 {
                return Some(map_darwin_thermal_level(state64));
            }
            unsafe {
                let _ = notify_cancel(tok);
            }
            *guard = None;
        }
        None
    }

    fn cached_host_port() -> u32 {
        // Cache mach_host_self() once to avoid leaking Mach send rights across probes.
        static HOST_PORT: OnceLock<u32> = OnceLock::new();
        *HOST_PORT.get_or_init(|| unsafe { mach_host_self() })
    }

    pub fn memory_summary() -> MemorySummary {
        const HOST_VM_INFO64: i32 = 4;
        let total_bytes = sysctl_u64("hw.memsize").unwrap_or(16 * 1073741824);
        let mut page_size: usize = 16384;
        let mut vm = VmStatistics64::default();
        // Matches HOST_VM_INFO64_REV0_COUNT (38 integer_t words); kernel caps writes to `count`.
        let mut count = (std::mem::size_of::<VmStatistics64>() / std::mem::size_of::<i32>()) as u32;
        let host = cached_host_port();
        unsafe {
            let _ = host_page_size(host, &mut page_size);
        }
        let rc = unsafe {
            host_statistics64(
                host,
                HOST_VM_INFO64,
                (&mut vm as *mut VmStatistics64) as *mut i32,
                &mut count,
            )
        };
        if rc != 0 {
            return MemorySummary {
                total_gb: round1(total_bytes as f64 / 1073741824.0),
                used_gb: 0.0,
                compressed_gb: 0.0,
                used_pct: 0,
            };
        }
        let ps = page_size as u64;
        let active = (vm.active_count as u64) * ps;
        let wired = (vm.wire_count as u64) * ps;
        let compressed = (vm.compressor_page_count as u64) * ps;
        let used = total_bytes.min(active.saturating_add(wired).saturating_add(compressed));
        let used_pct = if total_bytes > 0 {
            ((used as f64 / total_bytes as f64) * 100.0)
                .round()
                .min(100.0) as u32
        } else {
            0
        };
        MemorySummary {
            total_gb: round1(total_bytes as f64 / 1073741824.0),
            used_gb: round1(used as f64 / 1073741824.0),
            compressed_gb: round1(compressed as f64 / 1073741824.0),
            used_pct,
        }
    }

    type IoServiceMatchingFn = unsafe extern "C" fn(name: *const libc::c_char) -> *mut libc::c_void;
    type IoServiceGetMatchingServiceFn =
        unsafe extern "C" fn(main_port: u32, matching: *mut libc::c_void) -> u32;
    type IoRegistryEntryCreateCfPropertyFn = unsafe extern "C" fn(
        entry: u32,
        key: *const libc::c_void,
        allocator: *const libc::c_void,
        options: u32,
    ) -> *const libc::c_void;
    type IoObjectReleaseFn = unsafe extern "C" fn(object: u32) -> i32;
    type CfStringCreateWithCStringFn = unsafe extern "C" fn(
        alloc: *const libc::c_void,
        c_str: *const libc::c_char,
        encoding: u32,
    ) -> *const libc::c_void;
    type CfGetTypeIdFn = unsafe extern "C" fn(cf: *const libc::c_void) -> usize;
    type CfBooleanGetTypeIdFn = unsafe extern "C" fn() -> usize;
    type CfBooleanGetValueFn = unsafe extern "C" fn(boolean: *const libc::c_void) -> u8;
    type CfReleaseFn = unsafe extern "C" fn(cf: *const libc::c_void);

    struct IoKitApi {
        service_matching: IoServiceMatchingFn,
        get_matching_service: IoServiceGetMatchingServiceFn,
        create_cf_property: IoRegistryEntryCreateCfPropertyFn,
        object_release: IoObjectReleaseFn,
        cf_get_type_id: CfGetTypeIdFn,
        cf_boolean_type_id: CfBooleanGetTypeIdFn,
        cf_boolean_get_value: CfBooleanGetValueFn,
        cf_release: CfReleaseFn,
        clamshell_key: *const libc::c_void,
    }

    // Immutable CFStringRef + loaded system framework function pointers are safe to share.
    unsafe impl Send for IoKitApi {}
    unsafe impl Sync for IoKitApi {}

    unsafe fn load_sym(handle: *mut libc::c_void, name: &str) -> Option<*mut libc::c_void> {
        let c = CString::new(name).ok()?;
        let ptr = libc::dlsym(handle, c.as_ptr());
        if ptr.is_null() {
            None
        } else {
            Some(ptr)
        }
    }

    fn iokit_api() -> Option<&'static IoKitApi> {
        const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
        static API: OnceLock<Option<IoKitApi>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            let iokit_path =
                CString::new("/System/Library/Frameworks/IOKit.framework/IOKit").ok()?;
            let cf_path = CString::new(
                "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
            )
            .ok()?;
            let iokit = libc::dlopen(iokit_path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            let cf = libc::dlopen(cf_path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if iokit.is_null() || cf.is_null() {
                return None;
            }
            let service_matching = std::mem::transmute::<*mut libc::c_void, IoServiceMatchingFn>(
                load_sym(iokit, "IOServiceMatching")?,
            );
            let get_matching_service = std::mem::transmute::<
                *mut libc::c_void,
                IoServiceGetMatchingServiceFn,
            >(load_sym(iokit, "IOServiceGetMatchingService")?);
            let create_cf_property = std::mem::transmute::<
                *mut libc::c_void,
                IoRegistryEntryCreateCfPropertyFn,
            >(load_sym(iokit, "IORegistryEntryCreateCFProperty")?);
            let object_release = std::mem::transmute::<*mut libc::c_void, IoObjectReleaseFn>(
                load_sym(iokit, "IOObjectRelease")?,
            );
            let cf_str_create = std::mem::transmute::<
                *mut libc::c_void,
                CfStringCreateWithCStringFn,
            >(load_sym(cf, "CFStringCreateWithCString")?);
            let cf_get_type_id = std::mem::transmute::<*mut libc::c_void, CfGetTypeIdFn>(
                load_sym(cf, "CFGetTypeID")?,
            );
            let cf_boolean_type_id =
                std::mem::transmute::<*mut libc::c_void, CfBooleanGetTypeIdFn>(load_sym(
                    cf,
                    "CFBooleanGetTypeID",
                )?);
            let cf_boolean_get_value =
                std::mem::transmute::<*mut libc::c_void, CfBooleanGetValueFn>(load_sym(
                    cf,
                    "CFBooleanGetValue",
                )?);
            let cf_release = std::mem::transmute::<*mut libc::c_void, CfReleaseFn>(load_sym(
                cf,
                "CFRelease",
            )?);
            let key_c = CString::new("AppleClamshellState").ok()?;
            let clamshell_key = cf_str_create(std::ptr::null(), key_c.as_ptr(), K_CF_STRING_ENCODING_UTF8);
            if clamshell_key.is_null() {
                return None;
            }
            Some(IoKitApi {
                service_matching,
                get_matching_service,
                create_cf_property,
                object_release,
                cf_get_type_id,
                cf_boolean_type_id,
                cf_boolean_get_value,
                cf_release,
                clamshell_key,
            })
        })
        .as_ref()
    }

    /// Native sub-millisecond `AppleClamshellState` read via IOKit (`IOPMrootDomain`).
    /// Returns `Some(Some(closed))` on MacBooks, `Some(None)` on desktop Macs without a lid sensor,
    /// or `None` if IOKit FFI could not be queried (caller falls back to `/usr/sbin/ioreg`).
    pub fn lid_closed_iokit() -> Option<Option<bool>> {
        const DOMAIN: &[u8] = b"IOPMrootDomain\0";
        let api = iokit_api()?;
        unsafe {
            let matching = (api.service_matching)(DOMAIN.as_ptr() as *const libc::c_char);
            if matching.is_null() {
                return None;
            }
            // `IOServiceGetMatchingService` consumes one reference to `matching`.
            let service = (api.get_matching_service)(0, matching);
            if service == 0 {
                return None;
            }
            let prop = (api.create_cf_property)(service, api.clamshell_key, std::ptr::null(), 0);
            let _ = (api.object_release)(service);
            if prop.is_null() {
                return Some(None);
            }
            let val = if (api.cf_get_type_id)(prop) == (api.cf_boolean_type_id)() {
                Some((api.cf_boolean_get_value)(prop) != 0)
            } else {
                None
            };
            (api.cf_release)(prop);
            Some(val)
        }
    }
}

type LidProbe = (String, String);
type PowerProbe = (String, Option<u32>, String);
type ThermalProbe = (String, String, Option<u32>);

struct ProbeSources {
    lid: fn() -> Option<LidProbe>,
    power: fn() -> Option<PowerProbe>,
    thermal: fn() -> Option<ThermalProbe>,
    low_power: fn() -> Option<String>,
}

const PROBE_SOURCES: ProbeSources = ProbeSources {
    lid: probe_lid,
    power: probe_power,
    thermal: probe_thermal,
    low_power: probe_low_power,
};

#[derive(Default)]
pub struct MacOsProbeBackend {
    lid: ProbeCache<Option<LidProbe>>,
    power: ProbeCache<Option<PowerProbe>>,
    thermal: ProbeCache<Option<ThermalProbe>>,
    low_power: ProbeCache<Option<String>>,
}

fn mock_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn probe_lid() -> Option<LidProbe> {
    if let Some(mock) = mock_value("MAC_CLAMSHELL_MOCK_LID") {
        return Some((mock.trim().to_string(), "unknown".to_string()));
    }
    #[cfg(target_os = "macos")]
    {
        let (ok, out) = run_cmd_timeout(
            "/usr/sbin/ioreg",
            &["-r", "-c", "IOPMrootDomain", "-d", "1", "-w", "0"],
            CMD_TIMEOUT_MS,
        );
        ok.then(|| parse_ioreg_clamshell(&out))
    }
    #[cfg(not(target_os = "macos"))]
    None
}

fn probe_power() -> Option<PowerProbe> {
    if let Some(mock) = mock_value("MAC_CLAMSHELL_MOCK_POWER") {
        let pct = mock_value("MAC_CLAMSHELL_MOCK_BATT").and_then(|v| v.trim().parse().ok());
        return Some((mock, pct, String::new()));
    }
    let (ok, out) = probe_pmset(&["-g", "batt"])?;
    ok.then(|| parse_pmset_batt(&out))
}

fn mock_thermal() -> Option<ThermalProbe> {
    let therm = mock_value("MAC_CLAMSHELL_MOCK_THERM");
    let pressure = mock_value("MAC_CLAMSHELL_MOCK_THERMAL_PRESSURE");
    (therm.is_some() || pressure.is_some())
        .then(|| resolve_thermal_status(therm.as_deref(), pressure.as_deref(), None, ""))
}

fn probe_thermal() -> Option<ThermalProbe> {
    if let Some(mock) = mock_thermal() {
        return Some(mock);
    }
    let (ok, out) = probe_pmset(&["-g", "therm"])?;
    ok.then(|| resolve_thermal_status(None, None, native_thermal_pressure(), &out))
}

fn native_thermal_pressure() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        native::thermal_pressure_level()
    }
    #[cfg(not(target_os = "macos"))]
    None
}

fn probe_low_power() -> Option<String> {
    let (ok, out) = probe_pmset(&["-g"])?;
    ok.then(|| parse_low_power_mode(&out))
}

fn probe_pmset(args: &[&str]) -> Option<(bool, String)> {
    cfg!(target_os = "macos").then(|| run_cmd_timeout("/usr/bin/pmset", args, CMD_TIMEOUT_MS))
}

impl MacOsProbeBackend {
    fn compute_static_host() -> StaticHostInfo {
        #[cfg(target_os = "macos")]
        {
            let total = native::sysctl_u64("hw.ncpu")
                .or_else(|| native::sysctl_u64("hw.logicalcpu"))
                .unwrap_or(4)
                .max(1) as u32;
            let p_cores = native::sysctl_u64("hw.perflevel0.logicalcpu").unwrap_or(0) as u32;
            let raw_e = native::sysctl_u64("hw.perflevel1.logicalcpu").unwrap_or(0) as u32;
            let cpu_topology = build_cpu_topology(total, p_cores, raw_e);
            let default_chip = if cfg!(target_arch = "x86_64") {
                "Intel Mac"
            } else {
                "Apple Silicon"
            };
            StaticHostInfo {
                hostname: native::hostname(),
                chip: native::sysctl_string("machdep.cpu.brand_string")
                    .unwrap_or_else(|| default_chip.to_string()),
                macos_version: native::sysctl_string("kern.osproductversion")
                    .unwrap_or_else(|| "macOS".to_string()),
                cpu_topology,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            StaticHostInfo {
                hostname: "host".to_string(),
                chip: "unknown".to_string(),
                macos_version: std::env::consts::OS.to_string(),
                cpu_topology: CpuTopology {
                    total: 4,
                    performance_cores: 0,
                    efficiency_cores: 0,
                },
            }
        }
    }
}

impl ProbeBackend for MacOsProbeBackend {
    fn static_host(&self) -> StaticHostInfo {
        static HOST: OnceLock<StaticHostInfo> = OnceLock::new();
        let mut base = HOST.get_or_init(Self::compute_static_host).clone();
        if let Ok(mock_topo) = std::env::var("MAC_CLAMSHELL_MOCK_TOPOLOGY") {
            let trimmed = mock_topo.trim();
            if let Some(rest) = trimmed.strip_prefix("intel:") {
                if let Ok(cores) = rest.parse::<u32>() {
                    base.chip = "Intel Core i9".to_string();
                    base.cpu_topology = build_cpu_topology(cores, 0, 0);
                }
            } else if let Some(rest) = trimmed.strip_prefix("as:") {
                let parts: Vec<u32> = rest.split(':').filter_map(|p| p.parse().ok()).collect();
                if parts.len() == 2 {
                    base.chip = "Apple Silicon".to_string();
                    base.cpu_topology = build_cpu_topology(parts[0] + parts[1], parts[0], parts[1]);
                }
            }
        }
        base
    }

    fn lid_closed_fast(&self) -> Option<bool> {
        if let Ok(mock) = std::env::var("MAC_CLAMSHELL_MOCK_LID") {
            return match mock.trim() {
                "closed" => Some(true),
                "open" => Some(false),
                _ => None,
            };
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(res) = native::lid_closed_iokit() {
                return res;
            }
            let (ok, out) = run_cmd_timeout(
                "/usr/sbin/ioreg",
                &["-r", "-c", "IOPMrootDomain", "-d", "1", "-w", "0"],
                CMD_TIMEOUT_MS,
            );
            if !ok {
                return None;
            }
            let (lid, _) = parse_ioreg_clamshell(&out);
            match lid.as_str() {
                "closed" => Some(true),
                "open" => Some(false),
                _ => None,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn thermal_pressure_fast(&self) -> String {
        mock_thermal()
            .map(|(_, pressure, _)| pressure)
            .unwrap_or_else(|| native_thermal_pressure().unwrap_or("unknown").to_string())
    }

    fn probe_hardware(&self) -> HardwareProbe {
        self.collect_sources(false, &PROBE_SOURCES)
    }

    fn probe_hardware_fresh(&self) -> HardwareProbe {
        self.collect_sources(true, &PROBE_SOURCES)
    }
}

impl MacOsProbeBackend {
    fn collect_sources(&self, fresh: bool, sources: &ProbeSources) -> HardwareProbe {
        let lid = self
            .lid
            .sample(Duration::from_millis(1500), fresh, sources.lid);
        let power = self
            .power
            .sample(Duration::from_secs(4), fresh, sources.power);
        let thermal = self
            .thermal
            .sample(Duration::from_secs(4), fresh, sources.thermal);
        let low_power = self
            .low_power
            .sample(Duration::from_secs(12), fresh, sources.low_power);

        let unknown = || "unknown".to_string();
        let degraded = lid.is_none() || power.is_none() || low_power.is_none();
        let (lid_state, clamshell_causes_sleep) = lid.unwrap_or_else(|| (unknown(), unknown()));
        let (power_source, battery_pct, battery_detail) =
            power.unwrap_or_else(|| (unknown(), None, String::new()));
        let (thermal_state, thermal_pressure, cpu_speed_limit_pct) =
            thermal.unwrap_or_else(|| (unknown(), unknown(), None));
        let degraded = degraded || thermal_state == "unknown" || thermal_pressure == "unknown";
        let low_power_mode = low_power.unwrap_or_else(unknown);

        #[cfg(target_os = "macos")]
        let (memory, load_avg, uptime_seconds) = (
            native::memory_summary(),
            native::load_avg(),
            native::uptime_seconds(),
        );
        #[cfg(not(target_os = "macos"))]
        let (memory, load_avg, uptime_seconds) = (
            MemorySummary {
                total_gb: 0.0,
                used_gb: 0.0,
                compressed_gb: 0.0,
                used_pct: 0,
            },
            [0.0, 0.0, 0.0],
            0,
        );

        HardwareProbe {
            lid_state,
            clamshell_causes_sleep,
            power_source,
            battery_pct,
            battery_detail,
            thermal_state,
            thermal_pressure,
            cpu_speed_limit_pct,
            low_power_mode,
            memory,
            load_avg,
            uptime_seconds,
            degraded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_probe_sources_expire_independently_and_fresh_bypasses_each() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static CALLS: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
        let sources = ProbeSources {
            lid: || {
                CALLS[0].fetch_add(1, Ordering::Relaxed);
                Some(("open".into(), "no".into()))
            },
            power: || {
                CALLS[1].fetch_add(1, Ordering::Relaxed);
                Some(("ac".into(), Some(82), String::new()))
            },
            thermal: || {
                CALLS[2].fetch_add(1, Ordering::Relaxed);
                Some(resolve_thermal_status(None, None, Some("nominal"), ""))
            },
            low_power: || {
                CALLS[3].fetch_add(1, Ordering::Relaxed);
                Some("0".into())
            },
        };
        let counts = || std::array::from_fn(|i| CALLS[i].load(Ordering::Relaxed));
        let backend = MacOsProbeBackend::default();
        for _ in 0..5 {
            let hardware = backend.collect_sources(false, &sources);
            assert_eq!(hardware.lid_state, "open");
            assert_eq!(hardware.power_source, "ac");
            assert_eq!(hardware.thermal_pressure, "nominal");
            assert_eq!(hardware.low_power_mode, "0");
            assert!(!hardware.degraded);
        }
        assert_eq!(counts(), [1, 1, 1, 1]);

        let age = |at: &mut Instant| *at -= Duration::from_secs(5);
        age(&mut backend.lid.0.lock().unwrap().as_mut().unwrap().0);
        age(&mut backend.power.0.lock().unwrap().as_mut().unwrap().0);
        age(&mut backend.thermal.0.lock().unwrap().as_mut().unwrap().0);
        age(&mut backend.low_power.0.lock().unwrap().as_mut().unwrap().0);
        backend.collect_sources(false, &sources);
        assert_eq!(counts(), [2, 2, 2, 1]);

        backend.collect_sources(true, &sources);
        assert_eq!(counts(), [3, 3, 3, 2]);
    }

    #[test]
    fn thermal_failure_is_unknown_and_degrades_hardware_telemetry() {
        let mut sources = ProbeSources {
            lid: || Some(("open".into(), "yes".into())),
            power: || Some(("ac".into(), Some(75), String::new())),
            thermal: || None,
            low_power: || Some("0".into()),
        };
        let backend = MacOsProbeBackend::default();
        let assert_unknown = |probe: HardwareProbe| {
            assert_eq!(probe.thermal_state, "unknown");
            assert_eq!(probe.thermal_pressure, "unknown");
            assert_eq!(probe.cpu_speed_limit_pct, None);
            assert!(probe.degraded);
        };
        assert_unknown(backend.collect_sources(false, &sources));
        sources.thermal = || {
            Some(resolve_thermal_status(
                None,
                None,
                None,
                "unexpected output",
            ))
        };
        assert_unknown(backend.collect_sources(true, &sources));
    }

    #[test]
    fn parsers_thermal_ffi_and_topology_handle_apple_silicon_and_intel() {
        let ioreg = r#"
      "AppleClamshellCausesSleep" = No
      "AppleClamshellState" = Yes
"#;
        assert_eq!(
            parse_ioreg_clamshell(ioreg),
            ("closed".to_string(), "no".to_string())
        );

        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t93%; discharging; 8:30 remaining present: true\n";
        let (src, pct, detail) = parse_pmset_batt(batt);
        assert_eq!(src, "battery");
        assert_eq!(pct, Some(93));
        assert!(detail.starts_with("-InternalBattery-0"));

        // Apple Silicon pmset -g therm omits CPU_Speed_Limit -> None (never fake 100%)
        let as_therm = "Note: No thermal warning level has been recorded\nNote: No CPU power status has been recorded\n";
        assert_eq!(parse_pmset_therm(as_therm), ("ok".to_string(), None));

        // Intel XCPM pmset -g therm reports CPU_Speed_Limit
        let intel_therm = "CPU_Speed_Limit = 72\nCPU_Scheduler_Limit = 100\n";
        assert_eq!(
            parse_pmset_therm(intel_therm),
            ("warn".to_string(), Some(72))
        );

        // Darwin thermalpressurelevel mapping + combined resolution
        assert_eq!(map_darwin_thermal_level(0), "nominal");
        assert_eq!(map_darwin_thermal_level(1), "fair");
        assert_eq!(map_darwin_thermal_level(2), "serious");
        assert_eq!(map_darwin_thermal_level(3), "critical");

        assert_eq!(
            resolve_thermal_status(None, None, Some("serious"), as_therm),
            ("warn".to_string(), "serious".to_string(), None)
        );
        assert_eq!(
            resolve_thermal_status(None, None, Some("fair"), as_therm),
            ("ok".to_string(), "fair".to_string(), None)
        );

        // Intel homogeneous vs Apple Silicon heterogeneous topology
        let intel = build_cpu_topology(16, 0, 0);
        assert_eq!(intel.total, 16);
        assert_eq!(intel.performance_cores, 0);
        assert_eq!(intel.efficiency_cores, 0);

        let m3 = build_cpu_topology(14, 10, 4);
        assert_eq!(m3.total, 14);
        assert_eq!(m3.performance_cores, 10);
        assert_eq!(m3.efficiency_cores, 4);

        #[cfg(target_os = "macos")]
        {
            let lvl = native::thermal_pressure_level();
            assert!(
                matches!(lvl, Some("nominal" | "fair" | "serious" | "critical")),
                "expected Darwin com.apple.system.thermalpressurelevel, got {lvl:?}"
            );
            // Verify native IOKit AppleClamshellState FFI matches `ioreg` cleanly (MacBooks & desktop Macs).
            let iokit_lid = native::lid_closed_iokit();
            assert!(
                iokit_lid.is_some(),
                "expected native IOKit IOPMrootDomain query to succeed"
            );
            let (ok, out) = run_cmd_timeout(
                "/usr/sbin/ioreg",
                &["-r", "-c", "IOPMrootDomain", "-d", "1", "-w", "0"],
                CMD_TIMEOUT_MS,
            );
            if ok {
                let (lid_str, _) = parse_ioreg_clamshell(&out);
                let expected = match lid_str.as_str() {
                    "closed" => Some(true),
                    "open" => Some(false),
                    _ => None,
                };
                assert_eq!(iokit_lid.unwrap(), expected);
            }
        }

        let pm = " displaysleep 0\n lowpowermode 1\n";
        assert_eq!(parse_low_power_mode(pm), "1");
    }
}
