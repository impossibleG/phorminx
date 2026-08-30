use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACE_FILE_FLAGS,
    ReplaceFileW,
};
use windows::core::PCWSTR;

/// Atomically promotes a fully-written same-directory temporary file.
pub fn atomic_replace_file(temporary: &Path, destination: &Path) -> Result<(), AtomicReplaceError> {
    let temporary_wide = wide_path(temporary);
    let destination_wide = wide_path(destination);

    if destination.exists() {
        unsafe {
            ReplaceFileW(
                PCWSTR(destination_wide.as_ptr()),
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR::null(),
                REPLACE_FILE_FLAGS::default(),
                None,
                None,
            )
        }
        .map_err(|source| AtomicReplaceError::Replace {
            destination: destination.to_path_buf(),
            source,
        })
    } else {
        unsafe {
            MoveFileExW(
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR(destination_wide.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|source| AtomicReplaceError::Move {
            destination: destination.to_path_buf(),
            source,
        })
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum AtomicReplaceError {
    #[error("failed to replace settings file {destination}: {source}")]
    Replace {
        destination: PathBuf,
        source: windows::core::Error,
    },
    #[error("failed to move settings file into place at {destination}: {source}")]
    Move {
        destination: PathBuf,
        source: windows::core::Error,
    },
}
