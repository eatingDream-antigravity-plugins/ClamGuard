use crate::policy::CpuTopology;
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CMD_TIMEOUT_MS: u64 = 2500;

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
    fn lid_closed_fast(&self) -> Option<bool>;
    fn thermal_pressure_fast(&self) -> String {
        "nominal".to_string()
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
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return (false, String::new());
    };

    let Some(mut stdout_pipe) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return (false, String::new());
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
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
                poll_ms = (poll_ms * 2).min(8);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };

    let stdout = reader.join().unwrap_or_default();
    (ok, stdout)
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
    let mut thermal_state = "ok";
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
                    }
                }
            }
        }
    }

    if has_level && !no_warning {
        thermal_state = "warn";
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
            other => ("ok".to_string(), other.to_string(), None),
        };
    }

    let (pm_state, speed_limit) = parse_pmset_therm(pmset_out);
    let pressure = darwin_pressure
        .unwrap_or(if pm_state == "warn" { "serious" } else { "nominal" })
        .to_string();
    let state = if pm_state == "warn" || matches!(pressure.as_str(), "serious" | "critical") {
        "warn"
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

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

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
            ((used as f64 / total_bytes as f64) * 100.0).round().min(100.0) as u32
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
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacOsProbeBackend;

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
        let mock_pressure = std::env::var("MAC_CLAMSHELL_MOCK_THERMAL_PRESSURE")
            .ok()
            .filter(|s| !s.is_empty());
        let mock_therm = std::env::var("MAC_CLAMSHELL_MOCK_THERM")
            .ok()
            .filter(|s| !s.is_empty());
        if mock_pressure.is_some() || mock_therm.is_some() {
            let (_, pressure, _) =
                resolve_thermal_status(mock_therm.as_deref(), mock_pressure.as_deref(), None, "");
            return pressure;
        }
        #[cfg(target_os = "macos")]
        {
            native::thermal_pressure_level()
                .unwrap_or("nominal")
                .to_string()
        }
        #[cfg(not(target_os = "macos"))]
        {
            "nominal".to_string()
        }
    }

    fn probe_hardware(&self) -> HardwareProbe {
        let mock_lid = std::env::var("MAC_CLAMSHELL_MOCK_LID")
            .ok()
            .filter(|s| !s.is_empty());
        let mock_power = std::env::var("MAC_CLAMSHELL_MOCK_POWER")
            .ok()
            .filter(|s| !s.is_empty());
        let mock_batt = std::env::var("MAC_CLAMSHELL_MOCK_BATT")
            .ok()
            .filter(|s| !s.is_empty());
        let mock_therm = std::env::var("MAC_CLAMSHELL_MOCK_THERM")
            .ok()
            .filter(|s| !s.is_empty());
        let mock_pressure = std::env::var("MAC_CLAMSHELL_MOCK_THERMAL_PRESSURE")
            .ok()
            .filter(|s| !s.is_empty());

        let (ioreg_res, batt_res, therm_res, pmset_res) = std::thread::scope(|s| {
            let h_ioreg = s.spawn(|| {
                if mock_lid.is_some() || !cfg!(target_os = "macos") {
                    (true, String::new())
                } else {
                    run_cmd_timeout(
                        "/usr/sbin/ioreg",
                        &["-r", "-c", "IOPMrootDomain", "-d", "1", "-w", "0"],
                        CMD_TIMEOUT_MS,
                    )
                }
            });
            let h_batt = s.spawn(|| {
                if mock_power.is_some() || !cfg!(target_os = "macos") {
                    (true, String::new())
                } else {
                    run_cmd_timeout("/usr/bin/pmset", &["-g", "batt"], CMD_TIMEOUT_MS)
                }
            });
            let h_therm = s.spawn(|| {
                if mock_therm.is_some() || mock_pressure.is_some() || !cfg!(target_os = "macos") {
                    (true, String::new())
                } else {
                    run_cmd_timeout("/usr/bin/pmset", &["-g", "therm"], CMD_TIMEOUT_MS)
                }
            });
            let h_pmset = s.spawn(|| {
                if !cfg!(target_os = "macos") {
                    (true, String::new())
                } else {
                    run_cmd_timeout("/usr/bin/pmset", &["-g"], CMD_TIMEOUT_MS)
                }
            });
            (
                h_ioreg.join().unwrap_or((false, String::new())),
                h_batt.join().unwrap_or((false, String::new())),
                h_therm.join().unwrap_or((false, String::new())),
                h_pmset.join().unwrap_or((false, String::new())),
            )
        });

        let degraded = cfg!(target_os = "macos")
            && (!ioreg_res.0 || !batt_res.0 || !therm_res.0 || !pmset_res.0);

        let (lid_state, clamshell_causes_sleep) = if let Some(m) = mock_lid {
            (m, "unknown".to_string())
        } else {
            parse_ioreg_clamshell(&ioreg_res.1)
        };

        let (power_source, battery_pct, battery_detail) = if let Some(m) = mock_power {
            let pct = mock_batt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u32>().ok());
            (m, pct, String::new())
        } else {
            parse_pmset_batt(&batt_res.1)
        };

        #[cfg(target_os = "macos")]
        let darwin_pressure = if mock_therm.is_none() && mock_pressure.is_none() {
            native::thermal_pressure_level()
        } else {
            None
        };
        #[cfg(not(target_os = "macos"))]
        let darwin_pressure: Option<&'static str> = None;

        let (thermal_state, thermal_pressure, cpu_speed_limit_pct) = resolve_thermal_status(
            mock_therm.as_deref(),
            mock_pressure.as_deref(),
            darwin_pressure,
            &therm_res.1,
        );

        let low_power_mode = parse_low_power_mode(&pmset_res.1);

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
        assert_eq!(parse_pmset_therm(intel_therm), ("warn".to_string(), Some(72)));

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
        }

        let pm = " displaysleep 0\n lowpowermode 1\n";
        assert_eq!(parse_low_power_mode(pm), "1");
    }
}
