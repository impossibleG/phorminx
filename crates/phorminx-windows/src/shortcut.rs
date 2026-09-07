//! Canonical, layout-independent global shortcuts. No native calls are needed to validate them.
use std::fmt;

pub const DEFAULT_LAUNCHER_SHORTCUT: &str = "Ctrl+Alt+Space";
pub(crate) const CTRL: u8 = 1;
pub(crate) const ALT: u8 = 2;
pub(crate) const SHIFT: u8 = 4;
pub(crate) const WIN: u8 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shortcut {
    pub(crate) modifiers: u8,
    pub(crate) key: u16,
}

impl Shortcut {
    pub fn parse(text: &str) -> Result<Self, ShortcutError> {
        if text.len() > 80 {
            return Err(ShortcutError::Invalid);
        }
        let mut modifiers = 0;
        let mut key = None;
        for token in text.split('+') {
            let token = token.trim().to_ascii_uppercase();
            let modifier = match token.as_str() {
                "CTRL" | "CONTROL" => CTRL,
                "ALT" => ALT,
                "SHIFT" => SHIFT,
                "WIN" | "WINDOWS" | "SUPER" => WIN,
                _ => 0,
            };
            if modifier != 0 {
                if modifiers & modifier != 0 {
                    return Err(ShortcutError::Invalid);
                }
                modifiers |= modifier;
            } else {
                let value = match token.as_str() {
                    "SPACE" => 0x20,
                    "ENTER" | "RETURN" => 0x0d,
                    value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => {
                        u16::from(value.as_bytes()[0])
                    }
                    value if value.starts_with('F') => value[1..]
                        .parse::<u16>()
                        .ok()
                        .filter(|n| (1..=24).contains(n))
                        .map(|n| 0x6f + n)
                        .ok_or(ShortcutError::Invalid)?,
                    _ => return Err(ShortcutError::Invalid),
                };
                if key.replace(value).is_some() {
                    return Err(ShortcutError::Invalid);
                }
            }
        }
        let shortcut = Self {
            modifiers,
            key: key.ok_or(ShortcutError::Invalid)?,
        };
        if modifiers == 0 && !(0x70..=0x87).contains(&shortcut.key) {
            return Err(ShortcutError::NeedsModifier);
        }
        // Do not inject dummy keys into the destination to mask Alt/Win menus.
        if modifiers & WIN != 0 || modifiers & ALT != 0 && modifiers & CTRL == 0 {
            return Err(ShortcutError::UnsupportedModifier);
        }
        if modifiers == SHIFT && !(0x70..=0x87).contains(&shortcut.key) {
            return Err(ShortcutError::NeedsModifier);
        }
        // Never consume Windows security/shell shortcuts or common closing commands.
        if shortcut.key == 0x7b || (modifiers == CTRL && shortcut.key == 0x73) {
            return Err(ShortcutError::Reserved);
        }
        Ok(shortcut)
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bit, name) in [(CTRL, "Ctrl"), (ALT, "Alt"), (SHIFT, "Shift"), (WIN, "Win")] {
            if self.modifiers & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            0x20 => f.write_str("Space"),
            0x0d => f.write_str("Enter"),
            0x70..=0x87 => write!(f, "F{}", self.key - 0x6f),
            value => write!(f, "{}", char::from_u32(u32::from(value)).ok_or(fmt::Error)?),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortcutBindings {
    pub launcher: Shortcut,
    pub direct_dictation: Option<Shortcut>,
}

impl ShortcutBindings {
    pub fn parse(launcher: &str, direct: Option<&str>) -> Result<Self, ShortcutError> {
        let launcher = Shortcut::parse(launcher)?;
        let direct_dictation = direct
            .filter(|s| !s.trim().is_empty())
            .map(Shortcut::parse)
            .transpose()?;
        if direct_dictation == Some(launcher) {
            return Err(ShortcutError::Duplicate);
        }
        Ok(Self {
            launcher,
            direct_dictation,
        })
    }

    /// Snapshot check for registered global shortcuts. Application-local
    /// accelerators and registrations made by another app later cannot be detected.
    pub fn check_available(self) -> Result<(), ShortcutAvailabilityError> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
        };
        struct Registration(i32);
        impl Drop for Registration {
            fn drop(&mut self) {
                let _ = unsafe { UnregisterHotKey(None, self.0) };
            }
        }
        let mut registrations = Vec::new();
        for (index, shortcut) in std::iter::once(self.launcher)
            .chain(self.direct_dictation)
            .enumerate()
        {
            let id = 0x5f70 + index as i32;
            let bits = (if shortcut.modifiers & ALT != 0 { 1 } else { 0 })
                | (if shortcut.modifiers & CTRL != 0 { 2 } else { 0 })
                | u32::from(shortcut.modifiers & (SHIFT | WIN));
            unsafe {
                RegisterHotKey(
                    None,
                    id,
                    HOT_KEY_MODIFIERS(bits) | MOD_NOREPEAT,
                    u32::from(shortcut.key),
                )
            }
            .map_err(|_| ShortcutAvailabilityError {
                shortcut: shortcut.to_string(),
            })?;
            registrations.push(Registration(id));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "Windows could not reserve {shortcut}. It may already be registered by another application; choose another shortcut."
)]
pub struct ShortcutAvailabilityError {
    pub shortcut: String,
}

impl Default for ShortcutBindings {
    fn default() -> Self {
        Self {
            launcher: Shortcut {
                modifiers: CTRL | ALT,
                key: 0x20,
            },
            direct_dictation: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ShortcutError {
    #[error(
        "Choose a Ctrl combination or a function key. Ctrl can be combined with Alt and Shift."
    )]
    Invalid,
    #[error("Letters, numbers, Space, and Enter need Ctrl so ordinary typing stays available.")]
    NeedsModifier,
    #[error(
        "Windows-key and Alt-without-Ctrl shortcuts are not supported. Use Ctrl (optionally Alt/Shift), or a function key."
    )]
    UnsupportedModifier,
    #[error(
        "This shortcut is reserved by Windows or closes applications. Choose another combination."
    )]
    Reserved,
    #[error("The launcher and direct dictation shortcuts must be different.")]
    Duplicate,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonicalizes_and_round_trips() {
        for (raw, expected) in [
            (" alt + control + space ", "Ctrl+Alt+Space"),
            ("SHIFT+f24", "Shift+F24"),
            ("shift+ctrl+9", "Ctrl+Shift+9"),
            ("F8", "F8"),
            ("Ctrl+Return", "Ctrl+Enter"),
        ] {
            let parsed = Shortcut::parse(raw).unwrap();
            assert_eq!(parsed.to_string(), expected);
            assert_eq!(Shortcut::parse(&parsed.to_string()).unwrap(), parsed);
        }
    }
    #[test]
    fn rejects_ambiguous_unsafe_and_typing_shortcuts() {
        for value in [
            "",
            "Ctrl",
            "Ctrl+Ctrl+A",
            "Ctrl+A+B",
            "Ctrl++A",
            "A",
            "1",
            "Space",
            "Enter",
            "F25",
            "Alt+F4",
            "Win+L",
            "Ctrl+Alt+Delete",
            "Ctrl+🦀",
            "Win+F8",
            "Alt+A",
            "Shift+A",
            "Shift+Enter",
            "F12",
        ] {
            assert!(Shortcut::parse(value).is_err(), "{value}");
        }
    }
    #[test]
    fn duplicate_bindings_are_detected_after_normalization() {
        assert_eq!(
            ShortcutBindings::parse("Ctrl+Alt+Space", Some("alt+ctrl+space")),
            Err(ShortcutError::Duplicate)
        );
        assert_eq!(
            ShortcutBindings::parse(DEFAULT_LAUNCHER_SHORTCUT, Some("  ")).unwrap(),
            ShortcutBindings::default()
        );
    }
}
