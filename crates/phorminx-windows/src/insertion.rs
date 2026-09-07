use std::mem::size_of;
use std::thread;
use std::time::{Duration, Instant};

use arboard::{Clipboard, SetExtWindows};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};

use crate::TargetSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertionOutcome {
    Pasted,
    ClipboardOnly(ClipboardOnlyReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardOnlyReason {
    TargetUnavailable,
    TargetChanged,
    UnsupportedOrSensitiveTarget,
    ModifiersStillPressed,
    InputInjectionUncertain,
}

/// Places text on the clipboard and pastes only into a verified classic Win32
/// edit control. Clipboard restoration is intentionally disabled in Phase 1.
pub fn copy_and_maybe_paste(
    target: Option<TargetSnapshot>,
    text: &str,
) -> Result<InsertionOutcome, InsertionError> {
    let mut clipboard = SystemClipboard::new()?;
    let mut verifier = SystemTargetVerifier;
    let mut modifiers = SystemModifierRelease;
    let mut injector = SystemInputInjector;

    copy_and_maybe_paste_with(
        target,
        text,
        &mut clipboard,
        &mut verifier,
        &mut modifiers,
        &mut injector,
    )
}

fn copy_and_maybe_paste_with<T: Copy>(
    target: Option<T>,
    text: &str,
    clipboard: &mut impl ClipboardBackend,
    verifier: &mut impl TargetVerifier<T>,
    modifiers: &mut impl ModifierRelease,
    injector: &mut impl InputInjector,
) -> Result<InsertionOutcome, InsertionError> {
    clipboard.set_and_verify(text)?;

    let Some(target) = target else {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::TargetUnavailable,
        ));
    };
    if !verifier.is_current(target) {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::TargetChanged,
        ));
    }
    if !verifier.is_classic_writable_edit(target) {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::UnsupportedOrSensitiveTarget,
        ));
    }
    if !modifiers.wait_for_release(Duration::from_millis(750)) {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::ModifiersStillPressed,
        ));
    }
    if !verifier.is_current(target) || !verifier.is_classic_writable_edit(target) {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::TargetChanged,
        ));
    }

    if !injector.paste() {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::InputInjectionUncertain,
        ));
    }

    Ok(InsertionOutcome::Pasted)
}

trait ClipboardBackend {
    fn set_and_verify(&mut self, text: &str) -> Result<(), InsertionError>;
}

struct SystemClipboard(Clipboard);

impl SystemClipboard {
    fn new() -> Result<Self, InsertionError> {
        Ok(Self(Clipboard::new()?))
    }
}

impl ClipboardBackend for SystemClipboard {
    fn set_and_verify(&mut self, text: &str) -> Result<(), InsertionError> {
        self.0
            .set()
            .exclude_from_monitoring()
            .text(text.to_owned())?;
        if self.0.get_text()? != text {
            return Err(InsertionError::ClipboardVerification);
        }
        Ok(())
    }
}

trait TargetVerifier<T> {
    fn is_current(&mut self, target: T) -> bool;
    fn is_classic_writable_edit(&mut self, target: T) -> bool;
}

struct SystemTargetVerifier;

impl TargetVerifier<TargetSnapshot> for SystemTargetVerifier {
    fn is_current(&mut self, target: TargetSnapshot) -> bool {
        target.is_current()
    }

    fn is_classic_writable_edit(&mut self, target: TargetSnapshot) -> bool {
        target.is_classic_writable_edit()
    }
}

trait ModifierRelease {
    fn wait_for_release(&mut self, timeout: Duration) -> bool;
}

struct SystemModifierRelease;

impl ModifierRelease for SystemModifierRelease {
    fn wait_for_release(&mut self, timeout: Duration) -> bool {
        wait_for_modifier_release(timeout)
    }
}

trait InputInjector {
    fn paste(&mut self) -> bool;
}

struct SystemInputInjector;

impl InputInjector for SystemInputInjector {
    fn paste(&mut self) -> bool {
        let inputs = [
            key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
            key_input(VK_V, KEYBD_EVENT_FLAGS(0)),
            key_input(VK_V, KEYEVENTF_KEYUP),
            key_input(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        (unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) }) == inputs.len() as u32
    }
}

fn wait_for_modifier_release(timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if ![VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .into_iter()
            .chain(
                crate::hotkey::activation_keys()
                    .into_iter()
                    .filter(|key| *key != 0)
                    .map(VIRTUAL_KEY),
            )
            .any(key_is_down)
        {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn key_is_down(key: VIRTUAL_KEY) -> bool {
    (unsafe { GetAsyncKeyState(i32::from(key.0)) }) as u16 & 0x8000 != 0
}

fn key_input(key: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InsertionError {
    #[error("clipboard operation failed: {0}")]
    Clipboard(#[from] arboard::Error),
    #[error("clipboard readback did not match the Phorminx payload")]
    ClipboardVerification,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    const TARGET: u8 = 7;

    #[derive(Default)]
    struct FakeClipboard {
        calls: usize,
        fail: bool,
    }

    impl ClipboardBackend for FakeClipboard {
        fn set_and_verify(&mut self, _text: &str) -> Result<(), InsertionError> {
            self.calls += 1;
            if self.fail {
                Err(InsertionError::ClipboardVerification)
            } else {
                Ok(())
            }
        }
    }

    struct FakeVerifier {
        current: VecDeque<bool>,
        classic_edit: VecDeque<bool>,
        current_calls: usize,
        classic_edit_calls: usize,
    }

    impl FakeVerifier {
        fn new(
            current: impl IntoIterator<Item = bool>,
            classic_edit: impl IntoIterator<Item = bool>,
        ) -> Self {
            Self {
                current: current.into_iter().collect(),
                classic_edit: classic_edit.into_iter().collect(),
                current_calls: 0,
                classic_edit_calls: 0,
            }
        }
    }

    impl TargetVerifier<u8> for FakeVerifier {
        fn is_current(&mut self, target: u8) -> bool {
            assert_eq!(target, TARGET);
            self.current_calls += 1;
            self.current.pop_front().expect("unexpected current check")
        }

        fn is_classic_writable_edit(&mut self, target: u8) -> bool {
            assert_eq!(target, TARGET);
            self.classic_edit_calls += 1;
            self.classic_edit
                .pop_front()
                .expect("unexpected classic-edit check")
        }
    }

    struct FakeModifierRelease {
        released: bool,
        calls: usize,
    }

    impl ModifierRelease for FakeModifierRelease {
        fn wait_for_release(&mut self, timeout: Duration) -> bool {
            assert_eq!(timeout, Duration::from_millis(750));
            self.calls += 1;
            self.released
        }
    }

    struct FakeInjector {
        succeeds: bool,
        calls: usize,
    }

    impl InputInjector for FakeInjector {
        fn paste(&mut self) -> bool {
            self.calls += 1;
            self.succeeds
        }
    }

    fn modifiers(released: bool) -> FakeModifierRelease {
        FakeModifierRelease { released, calls: 0 }
    }

    fn injector(succeeds: bool) -> FakeInjector {
        FakeInjector { succeeds, calls: 0 }
    }

    #[test]
    fn unavailable_target_is_clipboard_only_without_injection() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([], []);
        let mut modifiers = modifiers(true);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            None::<u8>,
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(
            result.unwrap(),
            InsertionOutcome::ClipboardOnly(ClipboardOnlyReason::TargetUnavailable)
        );
        assert_eq!(clipboard.calls, 1);
        assert_eq!(verifier.current_calls, 0);
        assert_eq!(modifiers.calls, 0);
        assert_eq!(injector.calls, 0);
    }

    #[test]
    fn stale_initial_target_is_clipboard_only_without_injection() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([false], []);
        let mut modifiers = modifiers(true);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(
            result.unwrap(),
            InsertionOutcome::ClipboardOnly(ClipboardOnlyReason::TargetChanged)
        );
        assert_eq!(verifier.classic_edit_calls, 0);
        assert_eq!(modifiers.calls, 0);
        assert_eq!(injector.calls, 0);
    }

    #[test]
    fn unsupported_or_sensitive_target_is_clipboard_only_without_injection() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([true], [false]);
        let mut modifiers = modifiers(true);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(
            result.unwrap(),
            InsertionOutcome::ClipboardOnly(ClipboardOnlyReason::UnsupportedOrSensitiveTarget)
        );
        assert_eq!(modifiers.calls, 0);
        assert_eq!(injector.calls, 0);
    }

    #[test]
    fn held_modifiers_are_clipboard_only_without_injection() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([true], [true]);
        let mut modifiers = modifiers(false);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(
            result.unwrap(),
            InsertionOutcome::ClipboardOnly(ClipboardOnlyReason::ModifiersStillPressed)
        );
        assert_eq!(modifiers.calls, 1);
        assert_eq!(injector.calls, 0);
    }

    #[test]
    fn target_changed_after_modifier_release_is_clipboard_only_without_injection() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([true, false], [true]);
        let mut modifiers = modifiers(true);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(
            result.unwrap(),
            InsertionOutcome::ClipboardOnly(ClipboardOnlyReason::TargetChanged)
        );
        assert_eq!(verifier.classic_edit_calls, 1);
        assert_eq!(injector.calls, 0);
    }

    #[test]
    fn target_that_becomes_unsafe_is_clipboard_only_without_injection() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([true, true], [true, false]);
        let mut modifiers = modifiers(true);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(
            result.unwrap(),
            InsertionOutcome::ClipboardOnly(ClipboardOnlyReason::TargetChanged)
        );
        assert_eq!(verifier.classic_edit_calls, 2);
        assert_eq!(injector.calls, 0);
    }

    #[test]
    fn clipboard_failure_stops_before_target_checks_or_injection() {
        let mut clipboard = FakeClipboard {
            calls: 0,
            fail: true,
        };
        let mut verifier = FakeVerifier::new([], []);
        let mut modifiers = modifiers(true);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert!(matches!(result, Err(InsertionError::ClipboardVerification)));
        assert_eq!(verifier.current_calls, 0);
        assert_eq!(modifiers.calls, 0);
        assert_eq!(injector.calls, 0);
    }

    #[test]
    fn uncertain_injection_is_reported_after_one_attempt() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([true, true], [true, true]);
        let mut modifiers = modifiers(true);
        let mut injector = injector(false);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(
            result.unwrap(),
            InsertionOutcome::ClipboardOnly(ClipboardOnlyReason::InputInjectionUncertain)
        );
        assert_eq!(injector.calls, 1);
    }

    #[test]
    fn verified_safe_target_is_pasted_once() {
        let mut clipboard = FakeClipboard::default();
        let mut verifier = FakeVerifier::new([true, true], [true, true]);
        let mut modifiers = modifiers(true);
        let mut injector = injector(true);

        let result = copy_and_maybe_paste_with(
            Some(TARGET),
            "transcript",
            &mut clipboard,
            &mut verifier,
            &mut modifiers,
            &mut injector,
        );

        assert_eq!(result.unwrap(), InsertionOutcome::Pasted);
        assert_eq!(clipboard.calls, 1);
        assert_eq!(verifier.current_calls, 2);
        assert_eq!(verifier.classic_edit_calls, 2);
        assert_eq!(modifiers.calls, 1);
        assert_eq!(injector.calls, 1);
    }
}
