use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

const SETUP_OPERATION_NAME: &str = "Local\\Phorminx.SetupTransaction.v1";

/// Cross-process lease for the one setup transaction allowed to mutate the
/// current user's Phorminx installation at a time.
///
/// Windows releases the kernel handle after process failure, so an abandoned
/// operation cannot permanently lock repair. The durable setup journal remains
/// responsible for rolling back transaction-owned staging on the next run.
pub struct SetupOperationLock {
    handle: HANDLE,
}

impl SetupOperationLock {
    pub fn try_acquire() -> Result<Self, SetupOperationLockError> {
        acquire_named(SETUP_OPERATION_NAME)
    }
}

fn acquire_named(name: &str) -> Result<SetupOperationLock, SetupOperationLockError> {
    if name.is_empty() || name.contains('\0') {
        return Err(SetupOperationLockError::InvalidName);
    }
    let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
        .map_err(SetupOperationLockError::Create)?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(SetupOperationLockError::Busy);
    }
    Ok(SetupOperationLock { handle })
}

impl Drop for SetupOperationLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetupOperationLockError {
    #[error("another Phorminx setup or repair transaction is already active")]
    Busy,
    #[error("the setup transaction lock name is invalid")]
    InvalidName,
    #[error("Windows could not create the setup transaction lock: {0}")]
    Create(windows::core::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_is_exclusive_and_crash_safe_by_handle_lifetime() {
        let name = format!(
            "Local\\Phorminx.SetupTest.{}.{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        );
        let first = acquire_named(&name).unwrap();
        assert!(matches!(
            acquire_named(&name),
            Err(SetupOperationLockError::Busy)
        ));
        drop(first);
        assert!(acquire_named(&name).is_ok());
    }
}
