use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
};
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

    /// Acquires the slot for one settings profile. `None` is the normal
    /// profile; an isolated `--config` profile passes a stable scope so a QA
    /// profile can run beside the installed app without taking its slot.
    pub fn acquire_for_profile(scope: Option<&str>) -> Result<Self, SingleInstanceError> {
        acquire_named(&instance_name(scope))
    }

    /// Waits for the previous process of the same profile to release its slot.
    /// A settings restart spawns its replacement while the old process may
    /// still be finishing cleanup; giving up immediately left no app running.
    pub fn acquire_waiting(
        scope: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<Self, SingleInstanceError> {
        let name = instance_name(scope);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match acquire_named(&name) {
                Err(SingleInstanceError::AlreadyRunning)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                result => return result,
            }
        }
    }
}

fn instance_name(scope: Option<&str>) -> String {
    match scope {
        Some(scope) => format!("{INSTANCE_NAME}.profile.{scope}"),
        None => INSTANCE_NAME.to_owned(),
    }
}

/// Shows and focuses the durable product shell owned by an existing process.
/// Returns false while that process is still starting or no shell is available.
pub fn activate_existing_window() -> bool {
    let title = wide("Phorminx");
    let Ok(window) = (unsafe { FindWindowW(None, PCWSTR(title.as_ptr())) }) else {
        return false;
    };
    unsafe {
        let _ = ShowWindow(
            window,
            if IsIconic(window).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        let _ = SetForegroundWindow(window);
    }
    true
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
    fn profile_scopes_are_independent_and_waiting_acquires_after_release() {
        let scope = format!("test{}", std::process::id());
        assert_eq!(instance_name(None), INSTANCE_NAME);
        assert_ne!(instance_name(Some(&scope)), INSTANCE_NAME);
        let first = SingleInstance::acquire_for_profile(Some(&scope)).unwrap();
        assert!(matches!(
            SingleInstance::acquire_waiting(Some(&scope), std::time::Duration::from_millis(150)),
            Err(SingleInstanceError::AlreadyRunning)
        ));
        let waiter_scope = scope.clone();
        let waiter = std::thread::spawn(move || {
            SingleInstance::acquire_waiting(Some(&waiter_scope), std::time::Duration::from_secs(5))
                .is_ok()
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(first);
        assert!(waiter.join().unwrap());
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
