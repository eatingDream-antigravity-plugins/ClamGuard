use super::ProbeCache;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FAN_TTL: Duration = Duration::from_secs(4);
const MAX_FANS: u8 = 4;
const MAX_RPM: u32 = 15_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanReading {
    pub id: u8,
    pub label: String,
    pub rpm: u32,
    pub manual: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanTelemetry {
    pub status: String,
    pub fans: Vec<FanReading>,
    pub smc_mode: String,
    pub controller: String,
    pub external_managers: Vec<String>,
    #[serde(default)]
    pub detected_monitors: Vec<String>,
    pub ownership_mode: String,
    pub read_only: bool,
    pub sampled_at_ms: u64,
    pub stale: bool,
    pub note: String,
}

pub trait FanSensorSource: Send + Sync {
    fn sample(&self, fresh: bool) -> FanTelemetry;
}

#[derive(Default)]
pub struct MacOsFanSensorSource {
    cache: ProbeCache<FanTelemetry>,
}

impl FanSensorSource for MacOsFanSensorSource {
    fn sample(&self, fresh: bool) -> FanTelemetry {
        self.sample_with(fresh, || {
            if let Ok(mock) = std::env::var("MAC_CLAMSHELL_MOCK_FAN") {
                return mock_sample(mock.trim());
            }

            #[cfg(target_os = "macos")]
            {
                let (managers, monitors) = darwin::detected_software();
                let sensors = darwin::Connection::open()
                    .map(|smc| smc::sample(|key| smc.read(key)))
                    .unwrap_or_default();
                sensors.telemetry(managers, monitors)
            }
            #[cfg(not(target_os = "macos"))]
            Sensors::default().telemetry(Vec::new(), Vec::new())
        })
    }
}

impl MacOsFanSensorSource {
    fn sample_with(&self, fresh: bool, probe: impl FnOnce() -> FanTelemetry) -> FanTelemetry {
        self.cache.sample(FAN_TTL, fresh, probe)
    }
}

#[cfg(test)]
pub(crate) struct TestFanSensor(pub &'static str);

#[cfg(test)]
impl FanSensorSource for TestFanSensor {
    fn sample(&self, _fresh: bool) -> FanTelemetry {
        mock_sample(self.0)
    }
}

#[derive(Default)]
struct Sensors {
    // None means the sensor data was unavailable. An empty Vec means FNum was zero.
    rpms: Option<Vec<u32>>,
    force_mask: Option<u16>,
    per_fan_manual: u16,
    per_fan_known: u16,
    count: u8,
}

impl Sensors {
    fn manual_mask(&self) -> u16 {
        self.force_mask.unwrap_or(0) | self.per_fan_manual
    }

    fn smc_mode(&self, status: &str) -> &'static str {
        if status == "fanless" {
            return "none";
        }
        if self.manual_mask() != 0 {
            return "manual";
        }
        let fan_count = self.count.min(MAX_FANS);
        let all_fans_mode_known =
            fan_count > 0 && (0..fan_count).all(|id| self.per_fan_known & (1 << id) != 0);
        if self.force_mask == Some(0) || all_fans_mode_known {
            "auto"
        } else {
            "unknown"
        }
    }

    fn telemetry(
        self,
        mut external_managers: Vec<String>,
        mut detected_monitors: Vec<String>,
    ) -> FanTelemetry {
        external_managers.sort_unstable();
        external_managers.dedup();
        detected_monitors.sort_unstable();
        detected_monitors.dedup();
        let status = match self.rpms.as_deref() {
            None => "unavailable",
            Some([]) => "fanless",
            Some(_) => "ok",
        };
        let manual_mask = self.manual_mask();
        let smc_mode = self.smc_mode(status);
        let fans = self
            .rpms
            .unwrap_or_default()
            .into_iter()
            .zip(0u8..)
            .map(|(rpm, id)| FanReading {
                id,
                label: match (self.count, id) {
                    (2, 0) => "Left".to_string(),
                    (2, 1) => "Right".to_string(),
                    _ => format!("Fan {}", id + 1),
                },
                rpm,
                manual: manual_mask & (1 << id) != 0,
            })
            .collect();

        // Only name a specific external controller when SMC confirms manual mode and a single
        // dedicated fan-control app is detected. Never present monitor-only apps as controllers
        // and never assume macOS auto when SMC mode could not be read.
        let controller = match smc_mode {
            "manual" => match external_managers.as_slice() {
                [single] => single.clone(),
                [] => "Manual (SMC)".to_string(),
                many => format!("Manual (SMC) ({})", many.join(", ")),
            },
            "auto" | "none" => "System (macOS)".to_string(),
            _ => "Unknown".to_string(),
        };

        let yield_mode = smc_mode == "manual" || !external_managers.is_empty();
        let ownership_mode = if yield_mode { "yield" } else { "observer" };
        let note = match (smc_mode, external_managers.is_empty(), detected_monitors.is_empty()) {
            ("none", _, _) => {
                "Mac Clamshell is read-only; no hardware fans detected (passive cooling).".to_string()
            }
            ("manual", false, _) => format!(
                "Mac Clamshell is read-only (SMC mode: manual); detected fan-control software: {}.",
                external_managers.join(", ")
            ),
            ("manual", true, _) => {
                "Mac Clamshell is read-only (SMC mode: manual); yields fan control.".to_string()
            }
            ("auto", false, _) => format!(
                "Mac Clamshell is read-only (SMC mode: auto); detected fan-control software: {}.",
                external_managers.join(", ")
            ),
            ("auto", true, false) => format!(
                "Mac Clamshell is read-only (SMC mode: auto); detected monitoring software: {}.",
                detected_monitors.join(", ")
            ),
            ("auto", true, true) => {
                "Mac Clamshell is read-only (SMC mode: auto) and never writes SMC fan keys.".to_string()
            }
            (_, false, _) => format!(
                "Mac Clamshell is read-only (SMC mode: unknown); detected fan-control software: {}.",
                external_managers.join(", ")
            ),
            (_, true, false) => format!(
                "Mac Clamshell is read-only (SMC mode: unknown); detected monitoring software: {}.",
                detected_monitors.join(", ")
            ),
            (_, true, true) => {
                "Mac Clamshell is read-only (SMC mode: unknown) and never writes SMC fan keys.".to_string()
            }
        };

        FanTelemetry {
            status: status.to_string(),
            fans,
            smc_mode: smc_mode.to_string(),
            controller,
            external_managers,
            detected_monitors,
            ownership_mode: ownership_mode.to_string(),
            read_only: true,
            sampled_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            stale: false,
            note,
        }
    }
}

fn mock_sample(raw: &str) -> FanTelemetry {
    let mut managers = Vec::new();
    let mut monitors = Vec::new();
    let mut sensors = Sensors::default();
    let speeds = if let Some(speeds) = raw.strip_prefix("active:") {
        sensors.force_mask = Some(0);
        Some(speeds)
    } else if let Some(speeds) = raw.strip_prefix("manual:") {
        sensors.force_mask = Some(1);
        Some(speeds)
    } else if let Some(speeds) = raw.strip_prefix("unknown_mode:") {
        Some(speeds)
    } else if let Some((name, speeds)) = raw.strip_prefix("yield:").and_then(|v| v.split_once(':'))
    {
        sensors.force_mask = Some(1);
        if !name.is_empty() {
            managers.push(name.to_string());
        }
        Some(speeds)
    } else if let Some((name, speeds)) =
        raw.strip_prefix("monitor:").and_then(|v| v.split_once(':'))
    {
        sensors.force_mask = Some(0);
        if !name.is_empty() {
            monitors.push(name.to_string());
        }
        Some(speeds)
    } else {
        None
    };
    if let Some(speeds) = speeds {
        sensors.rpms = speeds
            .split(',')
            .map(|v| v.trim().parse::<u32>().ok().filter(|&rpm| rpm <= MAX_RPM))
            .collect::<Option<Vec<_>>>()
            .filter(|rpms| rpms.len() <= usize::from(MAX_FANS));
        sensors.count = sensors.rpms.as_ref().map_or(0, |rpms| rpms.len() as u8);
    } else if raw == "fanless" {
        sensors.rpms = Some(Vec::new());
    }
    sensors.telemetry(managers, monitors)
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SoftwareKind {
    Controller,
    Monitor,
}

#[cfg(any(target_os = "macos", test))]
fn classify_process(path: &str, name: &str) -> Option<(SoftwareKind, &'static str)> {
    if path.contains("/Macs Fan Control.app/") || name == "MacsFanControl" {
        Some((SoftwareKind::Controller, "Macs Fan Control"))
    } else if path.contains("/TG Pro.app/") || name == "TGPro" {
        Some((SoftwareKind::Controller, "TG Pro"))
    } else if name == "smcFanControl" {
        Some((SoftwareKind::Controller, "smcFanControl"))
    } else if path.contains("/Stats.app/") {
        Some((SoftwareKind::Monitor, "Stats"))
    } else if path.contains("/iStat Menus") || name == "iStatMenus" {
        Some((SoftwareKind::Monitor, "iStat Menus"))
    } else if path.contains("/Fanny.app/") {
        Some((SoftwareKind::Monitor, "Fanny"))
    } else {
        None
    }
}

#[cfg(any(target_os = "macos", test))]
mod smc {
    use super::{Sensors, MAX_FANS, MAX_RPM};

    const SMC_CMD_READ_KEYINFO: u8 = 9;
    const SMC_CMD_READ_BYTES: u8 = 5;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct KeyInfo {
        size: u32,
        data_type: u32,
        attributes: u8,
        _padding: [u8; 3],
    }

    #[repr(C)]
    #[derive(Default)]
    pub(super) struct SMCKeyData {
        key: u32,
        version: [u8; 6],
        _version_padding: [u8; 2],
        power_limits: [u32; 4],
        key_info: KeyInfo,
        result: u8,
        status: u8,
        command: u8,
        _command_padding: u8,
        data32: u32,
        bytes: [u8; 32],
    }

    enum FanValue {
        CountOrMode(u8),
        ForceMask(u16),
        Rpm(u32),
    }

    #[derive(Clone, Copy)]
    pub(super) enum FanKey {
        Count,
        ForceMask,
        Actual(u8),
        Mode(u8),
    }

    impl FanKey {
        fn code(self) -> Option<u32> {
            let code = match self {
                Self::Count => *b"FNum",
                Self::ForceMask => *b"FS! ",
                Self::Actual(id) if id < MAX_FANS => [b'F', b'0' + id, b'A', b'c'],
                Self::Mode(id) if id < MAX_FANS => [b'F', b'0' + id, b'M', b'd'],
                Self::Actual(_) | Self::Mode(_) => return None,
            };
            Some(u32::from_be_bytes(code))
        }

        fn accepts(self, info: KeyInfo) -> bool {
            let data_type = info.data_type.to_be_bytes();
            match self {
                Self::Count | Self::Mode(_) => (&data_type, info.size) == (b"ui8 ", 1),
                Self::ForceMask => matches!((&data_type, info.size), (b"ui16" | b"hex_", 2)),
                Self::Actual(_) => matches!((&data_type, info.size), (b"flt ", 4) | (b"fpe2", 2)),
            }
        }
    }

    pub(super) struct Value {
        info: KeyInfo,
        bytes: [u8; 32],
    }

    pub(super) fn read_key(
        key: FanKey,
        call: impl Fn(&SMCKeyData) -> Option<SMCKeyData>,
    ) -> Option<Value> {
        let mut request = SMCKeyData {
            key: key.code()?,
            command: SMC_CMD_READ_KEYINFO,
            ..SMCKeyData::default()
        };
        let metadata = call(&request)?;
        if metadata.result != 0 || !key.accepts(metadata.key_info) {
            return None;
        }
        request.key_info.size = metadata.key_info.size;
        request.command = SMC_CMD_READ_BYTES;
        let data = call(&request)?;
        (data.result == 0).then_some(Value {
            info: metadata.key_info,
            bytes: data.bytes,
        })
    }

    fn decode(key: FanKey, value: Value) -> Option<FanValue> {
        if !key.accepts(value.info) {
            return None;
        }
        let bytes = value.bytes;
        match key {
            FanKey::Count | FanKey::Mode(_) => Some(FanValue::CountOrMode(bytes[0])),
            FanKey::ForceMask => Some(FanValue::ForceMask(u16::from_be_bytes([
                bytes[0], bytes[1],
            ]))),
            FanKey::Actual(_) => {
                let rpm = if value.info.data_type.to_be_bytes() == *b"flt " {
                    f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                } else {
                    f32::from(u16::from_be_bytes([bytes[0], bytes[1]]) >> 2)
                };
                (0.0..=MAX_RPM as f32)
                    .contains(&rpm)
                    .then(|| FanValue::Rpm(rpm.round() as u32))
            }
        }
    }

    pub(super) fn sample(mut read: impl FnMut(FanKey) -> Option<Value>) -> Sensors {
        let mut get = |key| read(key).and_then(|value| decode(key, value));
        let force_mask = match get(FanKey::ForceMask) {
            Some(FanValue::ForceMask(mask)) => Some(mask),
            _ => None,
        };
        let Some(FanValue::CountOrMode(count)) = get(FanKey::Count) else {
            return Sensors {
                force_mask,
                ..Sensors::default()
            };
        };
        let mut per_fan_manual = 0u16;
        let mut per_fan_known = 0u16;
        for id in 0..count.min(MAX_FANS) {
            match get(FanKey::Mode(id)) {
                Some(FanValue::CountOrMode(1)) => {
                    per_fan_manual |= 1 << id;
                    per_fan_known |= 1 << id;
                }
                Some(FanValue::CountOrMode(0)) => {
                    per_fan_known |= 1 << id;
                }
                _ => {}
            }
        }
        let rpms = (0..count.min(MAX_FANS))
            .map(|id| match get(FanKey::Actual(id)) {
                Some(FanValue::Rpm(rpm)) => Some(rpm),
                _ => None,
            })
            .collect();
        Sensors {
            rpms,
            force_mask,
            per_fan_manual,
            per_fan_known,
            count,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::RefCell;
        use std::mem::{offset_of, size_of};

        fn value(data_type: &[u8; 4], bytes: &[u8]) -> Value {
            let mut data = [0; 32];
            data[..bytes.len()].copy_from_slice(bytes);
            Value {
                info: KeyInfo {
                    data_type: u32::from_be_bytes(*data_type),
                    size: bytes.len() as u32,
                    ..KeyInfo::default()
                },
                bytes: data,
            }
        }

        #[test]
        fn smc_abi_and_read_only_key_validation() {
            assert_eq!(size_of::<SMCKeyData>(), 80);
            assert_eq!(offset_of!(SMCKeyData, key_info), 28);
            assert_eq!(offset_of!(SMCKeyData, result), 40);
            assert_eq!(offset_of!(SMCKeyData, command), 42);
            assert_eq!(offset_of!(SMCKeyData, data32), 44);
            assert_eq!(offset_of!(SMCKeyData, bytes), 48);

            let commands = RefCell::new(Vec::new());
            let output = read_key(FanKey::Actual(3), |request| {
                commands.borrow_mut().push(request.command);
                assert_eq!(request.key.to_be_bytes(), *b"F3Ac");
                Some(match request.command {
                    9 => SMCKeyData {
                        key_info: value(b"fpe2", &[0; 2]).info,
                        ..SMCKeyData::default()
                    },
                    5 => {
                        assert_eq!(request.key_info.size, 2);
                        SMCKeyData {
                            bytes: value(b"fpe2", &(2210u16 << 2).to_be_bytes()).bytes,
                            ..SMCKeyData::default()
                        }
                    }
                    other => panic!("unexpected SMC command {other}"),
                })
            });
            assert_eq!(&*commands.borrow(), &[9, 5]);
            assert!(matches!(
                decode(FanKey::Actual(3), output.unwrap()),
                Some(FanValue::Rpm(2210))
            ));
            assert!(read_key(FanKey::Mode(4), |_| panic!("key outside allowlist")).is_none());
            assert!(read_key(FanKey::Actual(4), |_| panic!("key outside allowlist")).is_none());
            for metadata in [
                SMCKeyData {
                    result: 1,
                    key_info: value(b"ui8 ", &[2]).info,
                    ..SMCKeyData::default()
                },
                SMCKeyData {
                    key_info: value(b"ui8 ", &[2, 0]).info,
                    ..SMCKeyData::default()
                },
                SMCKeyData {
                    key_info: value(b"hex_", &[2]).info,
                    ..SMCKeyData::default()
                },
            ] {
                assert!(read_key(FanKey::Count, |request| {
                    assert_eq!(request.command, 9);
                    Some(SMCKeyData {
                        result: metadata.result,
                        key_info: metadata.key_info,
                        ..SMCKeyData::default()
                    })
                })
                .is_none());
            }
            assert!(read_key(FanKey::Count, |request| Some(SMCKeyData {
                key_info: value(b"ui8 ", &[2]).info,
                result: u8::from(request.command == 5),
                ..SMCKeyData::default()
            }))
            .is_none());
        }

        #[test]
        fn smc_sampling_decodes_rpm_and_both_manual_indicators() {
            let fanless = sample(|key| match key {
                FanKey::Count => Some(value(b"ui8 ", &[0])),
                _ => None,
            })
            .telemetry(Vec::new(), Vec::new());
            assert_eq!(fanless.status, "fanless");
            assert_eq!(fanless.smc_mode, "none");

            let dual = sample(|key| match key {
                FanKey::Count => Some(value(b"ui8 ", &[2])),
                FanKey::ForceMask => Some(value(b"hex_", &[0, 1])),
                FanKey::Mode(1) => Some(value(b"ui8 ", &[1])),
                FanKey::Mode(0) => Some(value(b"ui8 ", &[3])),
                FanKey::Actual(0) => Some(value(b"flt ", &2140.25f32.to_le_bytes())),
                FanKey::Actual(1) => Some(value(b"fpe2", &(2210u16 << 2).to_be_bytes())),
                _ => panic!("key outside expected fan range"),
            })
            .telemetry(Vec::new(), Vec::new());
            assert_eq!(dual.status, "ok");
            assert_eq!(dual.smc_mode, "manual");
            assert_eq!(dual.controller, "Manual (SMC)");
            assert_eq!(dual.ownership_mode, "yield");
            assert_eq!(
                dual.fans
                    .iter()
                    .map(|fan| (fan.rpm, fan.manual))
                    .collect::<Vec<_>>(),
                [(2140, true), (2210, true)]
            );

            // Apple Silicon without FS! key: F0Md=0 & F1Md=0 confirms auto; missing both => unknown
            let as_auto = sample(|key| match key {
                FanKey::Count => Some(value(b"ui8 ", &[2])),
                FanKey::ForceMask => None,
                FanKey::Mode(0 | 1) => Some(value(b"ui8 ", &[0])),
                FanKey::Actual(0) => Some(value(b"flt ", &2100.0f32.to_le_bytes())),
                FanKey::Actual(1) => Some(value(b"flt ", &2200.0f32.to_le_bytes())),
                _ => None,
            })
            .telemetry(Vec::new(), Vec::new());
            assert_eq!(as_auto.smc_mode, "auto");
            assert_eq!(as_auto.controller, "System (macOS)");

            let unknown_mode = sample(|key| match key {
                FanKey::Count => Some(value(b"ui8 ", &[2])),
                FanKey::Actual(0) => Some(value(b"flt ", &2100.0f32.to_le_bytes())),
                FanKey::Actual(1) => Some(value(b"flt ", &2200.0f32.to_le_bytes())),
                _ => None,
            })
            .telemetry(Vec::new(), Vec::new());
            assert_eq!(unknown_mode.status, "ok");
            assert_eq!(unknown_mode.smc_mode, "unknown");
            assert_eq!(unknown_mode.controller, "Unknown");

            for invalid in [f32::NAN, f32::INFINITY, -1.0, 15_001.0] {
                let unavailable = sample(|key| match key {
                    FanKey::Count => Some(value(b"ui8 ", &[1])),
                    FanKey::Actual(0) => Some(value(b"flt ", &invalid.to_le_bytes())),
                    _ => None,
                })
                .telemetry(Vec::new(), Vec::new());
                assert_eq!(unavailable.status, "unavailable");
                assert_eq!(unavailable.smc_mode, "unknown");
                assert!(unavailable.read_only);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod darwin {
    use super::smc;
    use std::ffi::c_void;
    use std::mem::size_of;

    const SMC_READ_SELECTOR: u32 = 2;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(name: *const libc::c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IOServiceOpen(service: u32, task: u32, connect_type: u32, conn: *mut u32) -> i32;
        fn IOObjectRelease(service: u32) -> i32;
        fn IOServiceClose(conn: u32) -> i32;
        fn IOConnectCallStructMethod(
            conn: u32,
            selector: u32,
            input: *const c_void,
            input_size: usize,
            output: *mut c_void,
            output_size: *mut usize,
        ) -> i32;
    }

    extern "C" {
        static mach_task_self_: u32;
    }

    pub(super) struct Connection(u32);

    impl Connection {
        pub(super) fn open() -> Option<Self> {
            unsafe {
                let matching = IOServiceMatching(c"AppleSMC".as_ptr());
                if matching.is_null() {
                    return None;
                }
                // IOKit consumes the matching dictionary, including on a failed match.
                let service = IOServiceGetMatchingService(0, matching);
                if service == 0 {
                    return None;
                }
                let mut conn = 0;
                let result = IOServiceOpen(service, mach_task_self_, 0, &mut conn);
                IOObjectRelease(service);
                (result == 0 && conn != 0).then(|| Self(conn))
            }
        }

        pub(super) fn read(&self, key: smc::FanKey) -> Option<smc::Value> {
            smc::read_key(key, |request| {
                let mut response = smc::SMCKeyData::default();
                let size = size_of::<smc::SMCKeyData>();
                let mut out_size = size;
                let result = unsafe {
                    IOConnectCallStructMethod(
                        self.0,
                        SMC_READ_SELECTOR,
                        std::ptr::from_ref(request).cast(),
                        size,
                        std::ptr::from_mut(&mut response).cast(),
                        &mut out_size,
                    )
                };
                (result == 0 && out_size == size).then_some(response)
            })
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            unsafe { IOServiceClose(self.0) };
        }
    }

    pub(super) fn detected_software() -> (Vec<String>, Vec<String>) {
        let estimated = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if estimated <= 0 {
            return (Vec::new(), Vec::new());
        }
        // Allow the process table to grow between the size query and the actual read.
        let mut pids = vec![0i32; estimated as usize + 32];
        let count = loop {
            let Ok(bytes) = i32::try_from(std::mem::size_of_val(pids.as_slice())) else {
                return (Vec::new(), Vec::new());
            };
            let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) }.max(0);
            if (count as usize) < pids.len() {
                break count as usize;
            }
            pids.resize(pids.len() * 2, 0);
        };

        let mut managers = Vec::new();
        let mut monitors = Vec::new();
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let mut push_match = |(kind, label): (super::SoftwareKind, &'static str)| match kind {
            super::SoftwareKind::Controller => managers.push(label.to_string()),
            super::SoftwareKind::Monitor => monitors.push(label.to_string()),
        };
        for pid in pids.into_iter().take(count).filter(|&pid| pid > 0) {
            if let Some(path) = proc_text(pid, &mut buf, libc::proc_pidpath) {
                let name = path.rsplit('/').next().unwrap_or_default();
                if let Some(matched) = super::classify_process(path, name) {
                    push_match(matched);
                }
            } else if let Some(name) = proc_text(pid, &mut buf, libc::proc_name) {
                if let Some(matched) = super::classify_process("", name) {
                    push_match(matched);
                }
            }
        }
        (managers, monitors)
    }

    fn proc_text(
        pid: i32,
        buf: &mut [u8],
        read: unsafe extern "C" fn(i32, *mut c_void, u32) -> i32,
    ) -> Option<&str> {
        let written = unsafe { read(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if written <= 0 {
            return None;
        }
        let data = buf.get(..written as usize)?;
        std::str::from_utf8(data.strip_suffix(&[0]).unwrap_or(data)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_cache_reuses_sensors_and_managers_until_expiry_or_fresh_request() {
        let source = MacOsFanSensorSource::default();
        let first = source.sample_with(false, || mock_sample("active:2140,2210"));
        for _ in 0..5 {
            assert_eq!(
                source.sample_with(false, || panic!(
                    "warm cache must skip fan and manager probes"
                )),
                first
            );
        }
        let fresh = source.sample_with(true, || mock_sample("yield:Macs Fan Control:2400,2450"));
        assert_eq!(fresh.controller, "Macs Fan Control");
        assert_eq!(fresh.fans[0].rpm, 2400);
        source.cache.0.lock().unwrap().as_mut().unwrap().0 -= FAN_TTL;
        let expired = source.sample_with(false, || mock_sample("unavailable"));
        assert_eq!(expired.status, "unavailable");
        assert!(expired.read_only);
    }

    #[test]
    fn mock_samples_report_labels_ownership_and_read_only_status() {
        let fanless = mock_sample("fanless");
        assert_eq!(
            (&*fanless.status, &*fanless.ownership_mode),
            ("fanless", "observer")
        );
        assert!(fanless.fans.is_empty());

        let dual = mock_sample("active:2140,2210");
        assert_eq!((&*dual.status, &*dual.controller), ("ok", "System (macOS)"));
        assert_eq!(dual.ownership_mode, "observer");
        assert_eq!(
            dual.fans
                .iter()
                .map(|f| (f.id, f.label.as_str(), f.rpm, f.manual))
                .collect::<Vec<_>>(),
            [(0, "Left", 2140, false), (1, "Right", 2210, false)]
        );
        assert_eq!(mock_sample("active:2140").fans[0].label, "Fan 1");
        assert_eq!(mock_sample("active:2140,2210,2200").fans[2].label, "Fan 3");

        let external = mock_sample("yield:Macs Fan Control:2400,2450");
        assert_eq!(external.external_managers, ["Macs Fan Control"]);
        assert_eq!(external.smc_mode, "manual");
        assert_eq!(external.controller, "Macs Fan Control");
        assert_eq!(external.ownership_mode, "yield");
        assert!(external
            .note
            .contains("detected fan-control software: Macs Fan Control"));

        let manual = mock_sample("manual:2140,2210");
        assert_eq!(manual.smc_mode, "manual");
        assert_eq!(manual.controller, "Manual (SMC)");
        assert_eq!(manual.ownership_mode, "yield");
        assert!(manual.fans[0].manual);
        assert!(!manual.fans[1].manual);

        let unknown_mode = mock_sample("unknown_mode:2140,2210");
        assert_eq!(unknown_mode.status, "ok");
        assert_eq!(unknown_mode.smc_mode, "unknown");
        assert_eq!(unknown_mode.controller, "Unknown");
        assert_eq!(unknown_mode.ownership_mode, "observer");

        let monitor = mock_sample("monitor:Fanny:2140,2210");
        assert_eq!(monitor.smc_mode, "auto");
        assert_eq!(monitor.controller, "System (macOS)");
        assert_eq!(monitor.ownership_mode, "observer");
        assert!(monitor.external_managers.is_empty());
        assert_eq!(monitor.detected_monitors, ["Fanny"]);

        let unavailable = mock_sample("unavailable");
        assert_eq!(unavailable.status, "unavailable");
        assert_eq!(unavailable.smc_mode, "unknown");
        assert_eq!(unavailable.controller, "Unknown");
        assert!(unavailable.fans.is_empty());
        for sample in [
            fanless,
            dual,
            external,
            manual,
            unknown_mode,
            monitor,
            unavailable,
        ] {
            assert!(sample.read_only);
            assert!(!sample.stale);
            assert!(sample.sampled_at_ms > 0);
        }
    }

    #[test]
    fn process_matching_separates_controllers_from_monitors_and_unknown_smc_mode() {
        let mut managers = Vec::new();
        let mut monitors = Vec::new();
        for (path, name) in [
            (
                "/Applications/Macs Fan Control.app/Contents/MacOS/helper",
                "",
            ),
            ("", "MacsFanControl"),
            ("/Applications/TG Pro.app/Contents/MacOS/helper", ""),
            ("", "TGPro"),
            ("/Applications/Stats.app/Contents/MacOS/Stats", "Stats"),
            ("/usr/local/bin/Stats", "Stats"),
            ("/Applications/iStat Menus.app/Contents/MacOS/helper", ""),
            ("", "iStatMenus"),
            ("", "smcFanControl"),
            ("/Applications/Fanny.app/Contents/MacOS/Fanny", "Fanny"),
            ("", "Fanny"),
        ] {
            if let Some((kind, label)) = classify_process(path, name) {
                match kind {
                    SoftwareKind::Controller => managers.push(label.to_string()),
                    SoftwareKind::Monitor => monitors.push(label.to_string()),
                }
            }
        }
        // When SMC mode failed to read, controller must remain "Unknown" (never Fanny or System)
        let unknown = Sensors::default().telemetry(managers.clone(), monitors.clone());
        assert_eq!(
            unknown.external_managers,
            ["Macs Fan Control", "TG Pro", "smcFanControl"]
        );
        assert_eq!(unknown.detected_monitors, ["Fanny", "Stats", "iStat Menus"]);
        assert_eq!(unknown.smc_mode, "unknown");
        assert_eq!(unknown.controller, "Unknown");
        assert_eq!(unknown.status, "unavailable");
        assert_eq!(unknown.ownership_mode, "yield");
        assert!(unknown.read_only);

        // Multiple controllers + confirmed manual SMC mode lists detected controllers without picking Fanny
        let multi_manual = Sensors {
            rpms: Some(vec![2400, 2450]),
            force_mask: Some(0x3),
            count: 2,
            ..Sensors::default()
        }
        .telemetry(managers, monitors.clone());
        assert_eq!(multi_manual.smc_mode, "manual");
        assert_eq!(
            multi_manual.controller,
            "Manual (SMC) (Macs Fan Control, TG Pro, smcFanControl)"
        );
        assert_eq!(multi_manual.ownership_mode, "yield");

        // Monitor-only apps alone never claim controller or yield ownership
        let monitor_only = Sensors {
            rpms: Some(vec![2100]),
            force_mask: Some(0),
            count: 1,
            ..Sensors::default()
        }
        .telemetry(Vec::new(), monitors);
        assert_eq!(monitor_only.smc_mode, "auto");
        assert_eq!(monitor_only.controller, "System (macOS)");
        assert_eq!(monitor_only.ownership_mode, "observer");
        assert!(monitor_only.external_managers.is_empty());
        assert_eq!(
            monitor_only.detected_monitors,
            ["Fanny", "Stats", "iStat Menus"]
        );
    }
}
