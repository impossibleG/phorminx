//! Replaces text Phorminx just pasted into a classic Win32 Edit control.
//!
//! Nothing is typed or undone. The control's current text is read back, the
//! exact inserted passage is located (preferring the one ending at the caret),
//! and only that range is selected and replaced. If the passage is no longer
//! there, unchanged and unambiguous, nothing is modified.

use std::ops::Range;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Controls::{EM_GETSEL, EM_REPLACESEL, EM_SETSEL};
use windows::Win32::UI::WindowsAndMessaging::{
    ES_PASSWORD, ES_READONLY, GWL_STYLE, GetClassNameW, GetWindowLongPtrW, IsWindow,
    SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SendMessageTimeoutW, WM_GETTEXT,
    WM_GETTEXTLENGTH,
};

use crate::TargetSnapshot;

/// Classic Edit controls cap their text well below this; larger reads are
/// refused rather than allocating without bound.
const MAX_TEXT_UNITS: usize = 4 * 1024 * 1024;
const MESSAGE_TIMEOUT_MS: u32 = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceOutcome {
    Replaced,
    /// The inserted passage was edited, removed or appears more than once.
    TextChanged,
    /// The control is gone, hidden behind a password style, or not editable.
    TargetUnavailable,
}

pub fn replace_inserted_text(
    target: TargetSnapshot,
    inserted: &str,
    replacement: &str,
) -> ReplaceOutcome {
    let control = HWND(target.focus_bits() as *mut core::ffi::c_void);
    if !unsafe { IsWindow(Some(control)) }.as_bool() || !is_writable_edit(control) {
        return ReplaceOutcome::TargetUnavailable;
    }
    let Some(text) = read_text(control) else {
        return ReplaceOutcome::TargetUnavailable;
    };
    let caret = selection_end(control);
    let Some(range) = locate_inserted(&text, inserted, caret) else {
        return ReplaceOutcome::TextChanged;
    };
    let replacement = edit_line_endings(replacement);
    let mut terminated = replacement.clone();
    terminated.push(0);
    let selected = send(
        control,
        EM_SETSEL,
        WPARAM(range.start),
        LPARAM(range.end as isize),
    );
    let replaced = selected.is_some()
        && send(
            control,
            EM_REPLACESEL,
            WPARAM(1),
            LPARAM(terminated.as_ptr() as isize),
        )
        .is_some();
    if !replaced {
        return ReplaceOutcome::TargetUnavailable;
    }
    match read_text(control) {
        Some(after)
            if after.get(range.start..range.start + replacement.len())
                == Some(&replacement[..]) =>
        {
            ReplaceOutcome::Replaced
        }
        _ => ReplaceOutcome::TextChanged,
    }
}

/// Finds the inserted passage in the control text (UTF-16). A match ending at
/// the caret wins; otherwise exactly one match is required.
fn locate_inserted(text: &[u16], inserted: &str, caret: Option<usize>) -> Option<Range<usize>> {
    let plain = inserted.encode_utf16().collect::<Vec<_>>();
    if plain.is_empty() {
        return None;
    }
    let converted = edit_line_endings(inserted);
    let mut matches = occurrences(text, &plain);
    if converted != plain {
        matches.extend(occurrences(text, &converted));
    }
    matches.sort_by_key(|range| range.start);
    matches.dedup();
    if let Some(caret) = caret
        && let Some(at_caret) = matches.iter().find(|range| range.end == caret)
    {
        return Some(at_caret.clone());
    }
    (matches.len() == 1).then(|| matches.remove(0))
}

fn occurrences(text: &[u16], needle: &[u16]) -> Vec<Range<usize>> {
    if needle.is_empty() || needle.len() > text.len() {
        return Vec::new();
    }
    (0..=text.len() - needle.len())
        .filter(|&start| text[start..start + needle.len()] == *needle)
        .map(|start| start..start + needle.len())
        .collect()
}

/// Edit controls store line breaks as CR LF.
fn edit_line_endings(text: &str) -> Vec<u16> {
    let mut output = Vec::with_capacity(text.len());
    let mut previous_cr = false;
    for unit in text.encode_utf16() {
        if unit == u16::from(b'\n') && !previous_cr {
            output.push(u16::from(b'\r'));
        }
        previous_cr = unit == u16::from(b'\r');
        output.push(unit);
    }
    output
}

fn is_writable_edit(control: HWND) -> bool {
    let mut class_name = [0_u16; 64];
    let length = unsafe { GetClassNameW(control, &mut class_name) };
    if length <= 0
        || !String::from_utf16_lossy(&class_name[..length as usize]).eq_ignore_ascii_case("Edit")
    {
        return false;
    }
    let style = unsafe { GetWindowLongPtrW(control, GWL_STYLE) } as u32;
    style & ES_PASSWORD as u32 == 0 && style & ES_READONLY as u32 == 0
}

fn read_text(control: HWND) -> Option<Vec<u16>> {
    let length = send(control, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0))?;
    if length > MAX_TEXT_UNITS {
        return None;
    }
    let mut buffer = vec![0_u16; length + 1];
    let copied = send(
        control,
        WM_GETTEXT,
        WPARAM(buffer.len()),
        LPARAM(buffer.as_mut_ptr() as isize),
    )?;
    buffer.truncate(copied.min(length));
    Some(buffer)
}

/// The caret position from the packed `EM_GETSEL` result. Positions past the
/// 16-bit packing limit are unknown rather than wrong.
fn selection_end(control: HWND) -> Option<usize> {
    let packed = send(control, EM_GETSEL, WPARAM(0), LPARAM(0))?;
    let end = (packed >> 16) & 0xFFFF;
    let length = send(control, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0))?;
    (length < 0xFFFF).then_some(end)
}

fn send(control: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<usize> {
    let mut result = 0_usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            control,
            message,
            wparam,
            lparam,
            SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
            MESSAGE_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    (sent.0 != 0).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn prefers_the_passage_ending_at_the_caret() {
        let text = utf16("hello world. hello world.");
        assert_eq!(
            locate_inserted(&text, "hello world.", Some(25)),
            Some(13..25)
        );
        assert_eq!(
            locate_inserted(&text, "hello world.", Some(12)),
            Some(0..12)
        );
        // Two matches and the caret elsewhere: ambiguous, so nothing changes.
        assert_eq!(locate_inserted(&text, "hello world.", Some(3)), None);
        assert_eq!(locate_inserted(&text, "hello world.", None), None);
    }

    #[test]
    fn unique_passage_matches_even_after_the_caret_moved() {
        let text = utf16("Dear team,\r\nThe launch moved.\r\nThanks");
        assert_eq!(
            locate_inserted(&text, "The launch moved.\nThanks", Some(0)),
            Some(12..37)
        );
        assert_eq!(locate_inserted(&text, "edited away", Some(0)), None);
        assert_eq!(locate_inserted(&text, "", Some(0)), None);
    }

    #[test]
    fn line_breaks_become_crlf_once() {
        assert_eq!(edit_line_endings("a\nb\r\nc"), utf16("a\r\nb\r\nc"));
    }
}
