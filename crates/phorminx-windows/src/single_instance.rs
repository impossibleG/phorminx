use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

const INSTANCE_NAME: &str = "Local\\Phorminx.v1";

/// Owns the machine-local, per-session Phorminx process mutex.
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// Acquires the Phorminx process slot. The handle must remain alive for the
    /// entire application lifetime.
    pub fn acquire() -> Result<Self, SingleInstanceError> {
        acquire_named(INSTANCE_NAME)
    }
}

fn acquire_named(name: &str) -> Result<SingleInstance, SingleInstanceError> {
    if name.is_empty() || name.contains('\0') {
        return Err(SingleInstanceError::InvalidName);
    }
    let name = wide(name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
        .map_err(SingleInstanceError::Create)?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(SingleInstanceError::AlreadyRunning);
    }
    Ok(SingleInstance { handle })
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum SingleInstanceError {
    #[error("Phorminx is already running for this Windows session")]
    AlreadyRunning,
    #[error("the single-instance mutex name is invalid")]
    InvalidName,
    #[error("Windows could not create the single-instance mutex: {0}")]
    Create(windows::core::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutex_is_exclusive_and_released_with_its_guard() {
        let name = format!(
            "Local\\Phorminx.test.{}.{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        );
        let first = acquire_named(&name).unwrap();
        assert!(matches!(
            acquire_named(&name),
            Err(SingleInstanceError::AlreadyRunning)
        ));
        drop(first);
        assert!(acquire_named(&name).is_ok());
    }

    #[test]
    fn names_are_validated_and_terminated() {
        assert!(matches!(
            acquire_named(""),
            Err(SingleInstanceError::InvalidName)
        ));
        assert_eq!(wide("A"), [65, 0]);
    }
}
