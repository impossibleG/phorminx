use std::fmt;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{PersistenceError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExecutableIdentity(String);

impl ExecutableIdentity {
    /// Creates an identity from an executable basename, never a filesystem path.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_executable(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecutableIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormattingStyle {
    Raw,
    Light,
    Balanced,
    Strong,
    Custom,
}

impl FormattingStyle {
    fn as_db(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Light => "light",
            Self::Balanced => "balanced",
            Self::Strong => "strong",
            Self::Custom => "custom",
        }
    }

    fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "raw" => Ok(Self::Raw),
            "light" => Ok(Self::Light),
            "balanced" => Ok(Self::Balanced),
            "strong" => Ok(Self::Strong),
            "custom" => Ok(Self::Custom),
            _ => Err(invalid_enum("formatting_style")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionPreference {
    Automatic,
    Direct,
    Clipboard,
}

impl InsertionPreference {
    fn as_db(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Direct => "direct",
            Self::Clipboard => "clipboard",
        }
    }

    fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "automatic" => Ok(Self::Automatic),
            "direct" => Ok(Self::Direct),
            "clipboard" => Ok(Self::Clipboard),
            _ => Err(invalid_enum("insertion_preference")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppProfile {
    pub executable: ExecutableIdentity,
    pub formatting_style: FormattingStyle,
    pub custom_instructions: Option<String>,
    pub language: Option<String>,
    pub insertion_preference: InsertionPreference,
    pub deny: bool,
}

pub struct AppProfileRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> AppProfileRepository<'connection> {
    pub(crate) fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    pub fn upsert(&self, profile: &AppProfile) -> Result<()> {
        validate_profile(profile)?;
        self.connection.execute(
            "INSERT INTO app_profiles(\
                 executable, formatting_style, custom_instructions, language, \
                 insertion_preference, deny\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(executable) DO UPDATE SET \
                 formatting_style = excluded.formatting_style, \
                 custom_instructions = excluded.custom_instructions, \
                 language = excluded.language, \
                 insertion_preference = excluded.insertion_preference, \
                 deny = excluded.deny",
            params![
                profile.executable.as_str(),
                profile.formatting_style.as_db(),
                profile.custom_instructions,
                profile.language,
                profile.insertion_preference.as_db(),
                profile.deny,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, executable: &ExecutableIdentity) -> Result<Option<AppProfile>> {
        Ok(self
            .connection
            .query_row(
                "SELECT executable, formatting_style, custom_instructions, language, \
                        insertion_preference, deny \
                 FROM app_profiles WHERE executable = ?1 COLLATE NOCASE",
                [executable.as_str()],
                map_profile,
            )
            .optional()?)
    }

    pub fn list(&self) -> Result<Vec<AppProfile>> {
        let mut statement = self.connection.prepare(
            "SELECT executable, formatting_style, custom_instructions, language, \
                    insertion_preference, deny \
             FROM app_profiles ORDER BY executable COLLATE NOCASE",
        )?;
        Ok(statement
            .query_map([], map_profile)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete(&self, executable: &ExecutableIdentity) -> Result<bool> {
        Ok(self.connection.execute(
            "DELETE FROM app_profiles WHERE executable = ?1 COLLATE NOCASE",
            [executable.as_str()],
        )? > 0)
    }
}

pub(crate) fn validate_executable(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(PersistenceError::Validation {
            field: "executable",
            reason: "must not be empty",
        });
    }
    if value.contains(['/', '\\']) || value.contains(':') {
        return Err(PersistenceError::Validation {
            field: "executable",
            reason: "must be a basename, not a path",
        });
    }
    Ok(())
}

fn validate_profile(profile: &AppProfile) -> Result<()> {
    if profile.formatting_style == FormattingStyle::Custom
        && profile
            .custom_instructions
            .as_deref()
            .is_none_or(|instructions| instructions.trim().is_empty())
    {
        return Err(PersistenceError::Validation {
            field: "custom_instructions",
            reason: "are required for a custom formatting style",
        });
    }
    Ok(())
}

fn map_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppProfile> {
    let executable: String = row.get(0)?;
    Ok(AppProfile {
        // Stored values passed validation on write. Keep decoding errors path-free.
        executable: ExecutableIdentity(executable),
        formatting_style: FormattingStyle::from_db(&row.get::<_, String>(1)?)?,
        custom_instructions: row.get(2)?,
        language: row.get(3)?,
        insertion_preference: InsertionPreference::from_db(&row.get::<_, String>(4)?)?,
        deny: row.get(5)?,
    })
}

fn invalid_enum(column: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, column.into(), rusqlite::types::Type::Text)
}
