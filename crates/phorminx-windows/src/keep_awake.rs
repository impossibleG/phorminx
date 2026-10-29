use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};

/// Asks Windows not to sleep (and optionally not to turn the screen off)
/// for as long as this value lives.
///
/// Nothing in the user's power plan changes and no input is simulated: a
/// dedicated thread holds a continuous execution-state request, the same
/// mechanism video players use. Windows drops the request when the thread
/// clears it or the process exits, so a crash cannot leave the PC awake.
/// Closing a laptop lid or pressing the power button still sleeps the PC.
pub struct KeepAwake {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl KeepAwake {
    pub fn start(keep_screen_on: bool) -> Result<Self, KeepAwakeError> {
        let (stop, stopped) = mpsc::channel::<()>();
        let (ready, started) = mpsc::channel::<bool>();
        let thread = thread::Builder::new()
            .name("phorminx-keep-awake".into())
            .spawn(move || {
                let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
                if keep_screen_on {
                    flags |= ES_DISPLAY_REQUIRED;
                }
                // A zero return means Windows refused the request.
                let accepted = unsafe { SetThreadExecutionState(flags) }.0 != 0;
                let _ = ready.send(accepted);
                if accepted {
                    // Blocks until the owner stops or drops this value.
                    let _ = stopped.recv();
                    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                }
            })
            .map_err(|_| KeepAwakeError::Thread)?;
        let mut keep_awake = Self { stop: Some(stop), thread: Some(thread) };
        match started.recv() {
            Ok(true) => Ok(keep_awake),
            _ => {
                keep_awake.release();
                Err(KeepAwakeError::Refused)
            }
        }
    }

    fn release(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeepAwakeError {
    #[error("Phorminx could not start its keep-awake helper")]
    Thread,
    #[error("Windows did not accept the keep-awake request")]
    Refused,
}
