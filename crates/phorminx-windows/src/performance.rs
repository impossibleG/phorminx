use std::ffi::OsStr;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME},
    System::{
        Power::{CallNtPowerInformation, PROCESSOR_POWER_INFORMATION, ProcessorInformation},
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Registry::{
            HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_64KEY, REG_DWORD, RegCloseKey,
            RegEnumValueW, RegOpenKeyExW,
        },
        SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
        Threading::{
            GetCurrentProcess, GetProcessInformation, GetProcessTimes, GetSystemTimes,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling,
        },
    },
};

const MIB: u64 = 1024 * 1024;
const VULKAN_DRIVERS_KEY: &str = r"SOFTWARE\Khronos\Vulkan\Drivers";
const MAX_VULKAN_DRIVER_MANIFESTS: u32 = 64;
const MAX_REGISTRY_NAME_UNITS: usize = 32_768;

/// A path-free snapshot used by the local performance benchmark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostResourceSnapshot {
    /// Current process working set. Candidate-local peak tracking must sample
    /// this value during the run; process-lifetime PeakWorkingSetSize is not a
    /// defensible per-candidate measurement.
    pub working_set_mib: u32,
    pub available_memory_mib: u32,
    /// Windows is actively reducing this process's execution speed.
    pub execution_speed_throttled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HostResourceError {
    #[error("Windows process memory information is unavailable")]
    ProcessMemory,
    #[error("Windows physical memory information is unavailable")]
    PhysicalMemory,
    #[error("Windows process power state is unavailable")]
    PowerState,
    #[error("Windows resource values exceed supported bounds")]
    NumericBounds,
    #[error("Windows Vulkan driver identity is unavailable")]
    VulkanDriverIdentity,
    #[error("Windows system processor load is unavailable")]
    ProcessorLoad,
    #[error("Windows system directory is unavailable")]
    SystemDirectory,
}

/// Measures aggregate CPU busy time over a bounded interval. Kernel time on
/// Windows includes idle time, so idle is subtracted from kernel + user.
pub fn host_cpu_busy_per_mille(interval: std::time::Duration) -> Result<u16, HostResourceError> {
    if interval.is_zero() || interval > std::time::Duration::from_secs(1) {
        return Err(HostResourceError::ProcessorLoad);
    }
    let before = system_times()?;
    std::thread::sleep(interval);
    let after = system_times()?;
    let idle = after
        .0
        .checked_sub(before.0)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let kernel = after
        .1
        .checked_sub(before.1)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let user = after
        .2
        .checked_sub(before.2)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let total = kernel
        .checked_add(user)
        .ok_or(HostResourceError::ProcessorLoad)?;
    if total == 0 || idle > total {
        return Err(HostResourceError::ProcessorLoad);
    }
    u16::try_from(total.saturating_sub(idle).saturating_mul(1_000) / total)
        .map_err(|_| HostResourceError::NumericBounds)
}

/// Measures CPU busy time not attributable to the current Phorminx process.
/// This prevents the recognizer under test from classifying its own expected
/// CPU work as hostile system contention.
pub fn host_external_cpu_busy_per_mille(
    interval: std::time::Duration,
) -> Result<u16, HostResourceError> {
    if interval.is_zero() || interval > std::time::Duration::from_secs(1) {
        return Err(HostResourceError::ProcessorLoad);
    }
    let process = unsafe { GetCurrentProcess() };
    let before_system = system_times()?;
    let before_process = process_times(process)?;
    std::thread::sleep(interval);
    let after_system = system_times()?;
    let after_process = process_times(process)?;
    external_cpu_ratio(before_system, after_system, before_process, after_process)
}

fn external_cpu_ratio(
    before_system: (u64, u64, u64),
    after_system: (u64, u64, u64),
    before_process: (u64, u64),
    after_process: (u64, u64),
) -> Result<u16, HostResourceError> {
    let idle = after_system
        .0
        .checked_sub(before_system.0)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let kernel = after_system
        .1
        .checked_sub(before_system.1)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let user = after_system
        .2
        .checked_sub(before_system.2)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let total = kernel
        .checked_add(user)
        .ok_or(HostResourceError::ProcessorLoad)?;
    if total == 0 || idle > total {
        return Err(HostResourceError::ProcessorLoad);
    }
    let process_kernel = after_process
        .0
        .checked_sub(before_process.0)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let process_user = after_process
        .1
        .checked_sub(before_process.1)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let process_busy = process_kernel
        .checked_add(process_user)
        .ok_or(HostResourceError::ProcessorLoad)?;
    let external_busy = total.saturating_sub(idle).saturating_sub(process_busy);
    u16::try_from(external_busy.saturating_mul(1_000) / total)
        .map_err(|_| HostResourceError::NumericBounds)
}

fn system_times() -> Result<(u64, u64, u64), HostResourceError> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all three pointers reference initialized writable storage.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }
        .map_err(|_| HostResourceError::ProcessorLoad)?;
    Ok((filetime(idle), filetime(kernel), filetime(user)))
}

fn process_times(
    process: windows::Win32::Foundation::HANDLE,
) -> Result<(u64, u64), HostResourceError> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the pseudo-handle is valid and every output points to initialized storage.
    unsafe {
        GetProcessTimes(
            process,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .map_err(|_| HostResourceError::ProcessorLoad)?;
    Ok((filetime(kernel), filetime(user)))
}

/// Resolves the native Windows system directory without trusting process
/// environment variables such as `SystemRoot`.
pub fn windows_system_directory() -> Result<PathBuf, HostResourceError> {
    let mut buffer = vec![0_u16; 32_768];
    // SAFETY: the buffer is initialized and writable for its declared length.
    let length = unsafe {
        windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buffer))
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(HostResourceError::SystemDirectory);
    }
    buffer.truncate(length);
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer));
    if !path.is_absolute() || !path.is_dir() {
        return Err(HostResourceError::SystemDirectory);
    }
    Ok(path)
}

const fn filetime(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

/// Returns enabled machine-wide Vulkan ICD manifest paths. The performance
/// host hashes the files before constructing a content-free identity.
pub fn vulkan_driver_manifests() -> Result<Vec<PathBuf>, HostResourceError> {
    let key_name = wide_null(OsStr::new(VULKAN_DRIVERS_KEY));
    let mut raw_key = HKEY::default();
    // SAFETY: key_name is NUL-terminated and raw_key is writable.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            windows::core::PCWSTR(key_name.as_ptr()),
            None,
            KEY_QUERY_VALUE | KEY_WOW64_64KEY,
            &mut raw_key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(Vec::new());
    }
    if status != ERROR_SUCCESS {
        return Err(HostResourceError::VulkanDriverIdentity);
    }
    let key = RegistryKey(raw_key);
    let mut paths = Vec::new();
    for index in 0..=MAX_VULKAN_DRIVER_MANIFESTS {
        let mut name = vec![0_u16; MAX_REGISTRY_NAME_UNITS];
        let mut name_units =
            u32::try_from(name.len()).map_err(|_| HostResourceError::VulkanDriverIdentity)?;
        let mut value_type = 0_u32;
        let mut state = [0_u8; size_of::<u32>()];
        let mut state_bytes =
            u32::try_from(state.len()).map_err(|_| HostResourceError::VulkanDriverIdentity)?;
        // SAFETY: all output buffers are valid for their supplied lengths.
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut name_units,
                None,
                Some(&mut value_type),
                Some(state.as_mut_ptr()),
                Some(&mut state_bytes),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            paths.sort();
            paths.dedup();
            return Ok(paths);
        }
        if status != ERROR_SUCCESS
            || value_type != REG_DWORD.0
            || state_bytes != size_of::<u32>() as u32
        {
            return Err(HostResourceError::VulkanDriverIdentity);
        }
        // A successful probe at the sentinel index proves the registry has
        // more entries than our identity bound, even when earlier entries were
        // disabled and therefore absent from `paths`.
        if index == MAX_VULKAN_DRIVER_MANIFESTS {
            return Err(HostResourceError::VulkanDriverIdentity);
        }
        if u32::from_le_bytes(state) != 0 {
            continue;
        }
        let name_units =
            usize::try_from(name_units).map_err(|_| HostResourceError::VulkanDriverIdentity)?;
        name.truncate(name_units);
        let path = PathBuf::from(
            String::from_utf16(&name).map_err(|_| HostResourceError::VulkanDriverIdentity)?,
        );
        if !path.is_absolute() || !path.is_file() {
            return Err(HostResourceError::VulkanDriverIdentity);
        }
        paths.push(path);
    }
    Err(HostResourceError::VulkanDriverIdentity)
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns the handle returned by RegOpenKeyExW.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

/// Reads process memory, system memory, and the OS execution-speed throttling
/// state without spawning a command or exposing host paths.
pub fn host_resource_snapshot() -> Result<HostResourceSnapshot, HostResourceError> {
    let process = unsafe { GetCurrentProcess() };
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>())
            .map_err(|_| HostResourceError::NumericBounds)?,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // SAFETY: the pseudo-handle is valid for this process and the initialized
    // structure is writable for exactly the byte count supplied.
    if !unsafe {
        K32GetProcessMemoryInfo(
            process,
            &mut counters,
            u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>())
                .map_err(|_| HostResourceError::NumericBounds)?,
        )
    }
    .as_bool()
    {
        return Err(HostResourceError::ProcessMemory);
    }

    let mut memory = MEMORYSTATUSEX {
        dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>())
            .map_err(|_| HostResourceError::NumericBounds)?,
        ..MEMORYSTATUSEX::default()
    };
    // SAFETY: memory is initialized and points to writable storage.
    unsafe { GlobalMemoryStatusEx(&mut memory) }.map_err(|_| HostResourceError::PhysicalMemory)?;

    Ok(HostResourceSnapshot {
        working_set_mib: bytes_to_mib(counters.WorkingSetSize as u64)?,
        available_memory_mib: bytes_to_mib(memory.ullAvailPhys)?,
        execution_speed_throttled: execution_speed_throttled(process)?,
    })
}

fn execution_speed_throttled(
    process: windows::Win32::Foundation::HANDLE,
) -> Result<bool, HostResourceError> {
    let mut power = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        ..PROCESS_POWER_THROTTLING_STATE::default()
    };
    // SAFETY: power is initialized and writable for the declared size.
    let process_result = unsafe {
        GetProcessInformation(
            process,
            ProcessPowerThrottling,
            (&raw mut power).cast(),
            u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>())
                .map_err(|_| HostResourceError::NumericBounds)?,
        )
    };
    if process_result.is_ok() {
        return Ok(
            power.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
                && power.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0,
        );
    }

    // Some desktop Windows builds do not expose ProcessPowerThrottling for a
    // normal Win32 process. Fall back to the machine processor MHz limit, an
    // OS-reported cap which includes active thermal/power throttling.
    let processors = std::thread::available_parallelism()
        .map_err(|_| HostResourceError::PowerState)?
        .get();
    let mut information = vec![PROCESSOR_POWER_INFORMATION::default(); processors];
    let byte_count = information
        .len()
        .checked_mul(size_of::<PROCESSOR_POWER_INFORMATION>())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(HostResourceError::NumericBounds)?;
    // SAFETY: information is writable for byte_count bytes and this query has
    // no input buffer.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            None,
            0,
            Some(information.as_mut_ptr().cast()),
            byte_count,
        )
    };
    if status.0 != 0
        || information
            .iter()
            .any(|processor| processor.MaxMhz == 0 || processor.MhzLimit == 0)
    {
        return Err(HostResourceError::PowerState);
    }
    Ok(information
        .iter()
        .any(|processor| processor.MhzLimit < processor.MaxMhz))
}

fn bytes_to_mib(bytes: u64) -> Result<u32, HostResourceError> {
    u32::try_from(bytes.div_ceil(MIB)).map_err(|_| HostResourceError::NumericBounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_snapshot_is_bounded_and_nonzero() {
        let snapshot = host_resource_snapshot().unwrap();
        assert!(snapshot.working_set_mib > 0);
        assert!(snapshot.available_memory_mib > 0);
    }

    #[test]
    fn byte_conversion_rounds_up_and_rejects_overflow() {
        assert_eq!(bytes_to_mib(1).unwrap(), 1);
        assert_eq!(bytes_to_mib(MIB).unwrap(), 1);
        assert_eq!(bytes_to_mib(MIB + 1).unwrap(), 2);
        assert!(bytes_to_mib(u64::MAX).is_err());
    }

    #[test]
    fn live_cpu_load_is_a_bounded_ratio() {
        let busy = host_cpu_busy_per_mille(std::time::Duration::from_millis(75)).unwrap();
        assert!(busy <= 1_000);
        assert!(host_cpu_busy_per_mille(std::time::Duration::ZERO).is_err());
        assert!(host_cpu_busy_per_mille(std::time::Duration::from_secs(2)).is_err());
        assert!(
            host_external_cpu_busy_per_mille(std::time::Duration::from_millis(75)).unwrap()
                <= 1_000
        );
    }

    #[test]
    fn candidate_process_cpu_is_subtracted_from_contention() {
        // total=1,000; idle=100; Phorminx=800 => external busy=100.
        assert_eq!(
            external_cpu_ratio((0, 0, 0), (100, 600, 400), (0, 0), (500, 300)).unwrap(),
            100
        );
    }

    #[test]
    fn system_directory_does_not_depend_on_environment() {
        let directory = windows_system_directory().unwrap();
        assert!(directory.is_absolute());
        assert!(directory.join("ntoskrnl.exe").is_file());
    }

    #[test]
    fn live_vulkan_manifest_inventory_is_sorted_unique_and_bounded() {
        let manifests = vulkan_driver_manifests().unwrap();
        assert!(manifests.len() <= MAX_VULKAN_DRIVER_MANIFESTS as usize);
        assert!(manifests.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            manifests
                .iter()
                .all(|path| path.is_absolute() && path.is_file())
        );
    }
}
