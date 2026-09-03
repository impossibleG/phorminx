use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;

/// Atomically promotes a fully-written same-directory temporary file.
pub fn atomic_replace_file(temporary: &Path, destination: &Path) -> Result<(), AtomicReplaceError> {
    let temporary_wide = wide_path(temporary);
    let destination_wide = wide_path(destination);

    // This is the supported Windows durability primitive for both first write
    // and replacement. FlushFileBuffers on directory handles is denied on
    // supported hosts, so no directory-flush failure is silently ignored.
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

/// Atomically activates a same-volume prepared directory without replacing an
/// existing target. MOVEFILE_WRITE_THROUGH makes Windows wait for the move to
/// reach durable storage before returning.
pub fn atomic_activate_directory(
    prepared: &Path,
    destination: &Path,
) -> Result<(), AtomicReplaceError> {
    if destination.exists() {
        return Err(AtomicReplaceError::DestinationExists(
            destination.to_path_buf(),
        ));
    }
    let prepared_wide = wide_path(prepared);
    let destination_wide = wide_path(destination);
    unsafe {
        MoveFileExW(
            PCWSTR(prepared_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|source| AtomicReplaceError::Move {
        destination: destination.to_path_buf(),
        source,
    })
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum AtomicReplaceError {
    #[error("the atomic activation destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("failed to move settings file into place at {destination}: {source}")]
    Move {
        destination: PathBuf,
        source: windows::core::Error,
    },
}
