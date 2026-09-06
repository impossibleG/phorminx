//! Only DPAPI ciphertext crosses the SQLite boundary. Recovery never stores audio.
use crate::{MAX_TERMINAL_TEXT_BYTES, PersistenceError, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryRecord {
    pub session: String,
    pub updated_at_ms: i64,
}

pub struct RecoveryRepository<'a> {
    connection: &'a Connection,
}
impl<'a> RecoveryRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }
    /// Called once by the exclusive application process before starting its
    /// writer. Old snapshots survive; old writer tokens and deletion markers
    /// cannot affect this process. Ordinary readers must not call this method.
    pub fn begin_process(&self) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("UPDATE persistence_settings SET value=CAST(value AS INTEGER)+1 WHERE key='recovery_epoch'", [])?;
        transaction.execute("DELETE FROM recovery_tombstones", [])?;
        transaction.commit()?;
        Ok(())
    }
    pub fn epoch(&self) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT CAST(value AS INTEGER) FROM persistence_settings WHERE key='recovery_epoch'",
            [],
            |r| r.get(0),
        )?)
    }
    /// One SQLite read snapshot captures permission and its revocation token
    /// together, before microphone capture begins.
    pub fn eligibility_epoch(&self) -> Result<Option<i64>> {
        Ok(self.connection.query_row(
            "SELECT CASE WHEN (SELECT value FROM persistence_settings WHERE key='history_retention') != 'disabled' THEN CAST(value AS INTEGER) END FROM persistence_settings WHERE key='recovery_epoch'",
            [],
            |row| row.get(0),
        )?)
    }
    /// The epoch captured before recognition prevents queued writes from undoing clear/disable.
    pub fn save(&self, session: &str, epoch: i64, updated_at_ms: i64, text: &str) -> Result<bool> {
        validate_session(session)?;
        if epoch < 0 || updated_at_ms < 0 {
            return Err(PersistenceError::Protection);
        }
        if text.is_empty() {
            return Ok(false);
        }
        if self.epoch()? != epoch
            || crate::HistoryRepository::new(self.connection).retention()?
                == crate::RetentionPolicy::Disabled
        {
            return Ok(false);
        }
        if text.len() > MAX_TERMINAL_TEXT_BYTES {
            return Err(PersistenceError::Protection);
        }
        let protected = protect(text.as_bytes(), false, session)?;
        Ok(self.connection.execute("INSERT INTO interrupted_dictation(session,updated_at_ms,protected_text) SELECT ?1,?2,?3 WHERE (SELECT value FROM persistence_settings WHERE key='history_retention') != 'disabled' AND (SELECT CAST(value AS INTEGER) FROM persistence_settings WHERE key='recovery_epoch')=?4 AND NOT EXISTS(SELECT 1 FROM recovery_tombstones WHERE session=?1) ON CONFLICT(session) DO UPDATE SET updated_at_ms=excluded.updated_at_ms, protected_text=excluded.protected_text WHERE excluded.updated_at_ms>=interrupted_dictation.updated_at_ms", params![session, updated_at_ms, protected, epoch])? != 0)
    }
    pub fn list(&self) -> Result<Vec<RecoveryRecord>> {
        let mut query = self.connection.prepare("SELECT session,updated_at_ms FROM interrupted_dictation WHERE (SELECT value FROM persistence_settings WHERE key='history_retention') != 'disabled' ORDER BY updated_at_ms DESC LIMIT 100")?;
        Ok(query
            .query_map([], |r| {
                Ok(RecoveryRecord {
                    session: r.get(0)?,
                    updated_at_ms: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn text(&self, session: &str) -> Result<Option<String>> {
        validate_session(session)?;
        let blob: Option<Vec<u8>> = self.connection.query_row("SELECT protected_text FROM interrupted_dictation WHERE session=?1 AND length(protected_text) <= ?2 AND (SELECT value FROM persistence_settings WHERE key='history_retention') != 'disabled'", params![session, MAX_TERMINAL_TEXT_BYTES + 16384], |r| r.get(0)).optional()?;
        blob.map(|bytes| {
            String::from_utf8(protect(&bytes, true, session)?)
                .map_err(|_| PersistenceError::Protection)
        })
        .transpose()
    }
    pub fn discard(&self, session: &str) -> Result<()> {
        validate_session(session)?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO recovery_tombstones(session) VALUES (?1)",
            [session],
        )?;
        transaction.execute(
            "DELETE FROM interrupted_dictation WHERE session=?1",
            [session],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn validate_session(session: &str) -> Result<()> {
    if session.is_empty()
        || session.len() > 128
        || !session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(PersistenceError::Protection);
    }
    Ok(())
}

#[cfg(windows)]
fn protect(bytes: &[u8], decrypt: bool, session: &str) -> Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| PersistenceError::Protection)?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // Bind the authenticated payload to its logical row. Swapping otherwise
    // valid encrypted rows must never return another dictation's text.
    let entropy_bytes = format!("Phorminx interrupted text v1:{session}").into_bytes();
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len() as u32,
        pbData: entropy_bytes.as_ptr().cast_mut(),
    };
    // No LOCAL_MACHINE flag: Windows binds this blob to the current user. Native
    // allocations are copied, wiped on decrypt, and released using LocalFree.
    unsafe {
        let result = if decrypt {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                windows::core::PCWSTR::null(),
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        result.map_err(|_| PersistenceError::Protection)?;
        if output.pbData.is_null() {
            return Err(PersistenceError::Protection);
        }
        let data = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let copy = (!decrypt || data.len() <= MAX_TERMINAL_TEXT_BYTES).then(|| data.to_vec());
        if decrypt {
            for byte in data {
                std::ptr::write_volatile(byte, 0);
            }
        }
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        copy.ok_or(PersistenceError::Protection)
    }
}

#[cfg(not(windows))]
fn protect(_bytes: &[u8], _decrypt: bool, _session: &str) -> Result<Vec<u8>> {
    Err(PersistenceError::Protection)
}

#[cfg(all(test, windows))]
mod tests {
    use crate::{Persistence, RetentionPolicy};
    #[test]
    fn unicode_is_encrypted_and_corruption_remains_visible_and_discardable() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("unicode.db")).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        let epoch = db.recovery().epoch().unwrap();
        let text = "Olá, ação brasileira. English — café 🦉\n第二行";
        db.recovery().save("unicode", epoch, 1, text).unwrap();
        assert_eq!(
            db.recovery().text("unicode").unwrap().as_deref(),
            Some(text)
        );
        let mut blob: Vec<u8> = db
            .connection
            .query_row(
                "SELECT protected_text FROM interrupted_dictation",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x80;
        db.connection
            .execute("UPDATE interrupted_dictation SET protected_text=?1", [blob])
            .unwrap();
        let error = db.recovery().text("unicode").unwrap_err();
        assert!(!format!("{error:?} {error}").contains(text));
        assert_eq!(db.recovery().list().unwrap().len(), 1);
        db.recovery().discard("unicode").unwrap();
        assert!(db.recovery().list().unwrap().is_empty());
    }

    #[test]
    fn encrypted_payload_cannot_be_moved_to_another_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("binding.db")).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        db.recovery()
            .save(
                "original",
                db.recovery().epoch().unwrap(),
                1,
                "bound payload",
            )
            .unwrap();
        db.connection
            .execute("UPDATE interrupted_dictation SET session='different'", [])
            .unwrap();
        assert!(db.recovery().text("different").is_err());
        assert_eq!(db.recovery().list().unwrap()[0].session, "different");
        db.recovery().discard("different").unwrap();
    }

    #[test]
    fn discard_and_restart_reject_delayed_writes_from_another_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("concurrent.db");
        let ui = Persistence::open(&path).unwrap();
        let writer = Persistence::open(&path).unwrap();
        ui.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        let epoch = writer.recovery().epoch().unwrap();
        writer
            .recovery()
            .save("discarded", epoch, 1, "old text")
            .unwrap();
        ui.recovery().discard("discarded").unwrap();
        assert!(
            !writer
                .recovery()
                .save("discarded", epoch, 2, "queued stale text")
                .unwrap()
        );
        writer
            .recovery()
            .save("survivor", epoch, 3, "recover after restart")
            .unwrap();
        ui.recovery().begin_process().unwrap();
        assert!(
            !writer
                .recovery()
                .save("discarded", epoch, 4, "obsolete writer")
                .unwrap()
        );
        assert_eq!(
            ui.recovery().text("survivor").unwrap().as_deref(),
            Some("recover after restart")
        );
        let markers: i64 = ui
            .connection
            .query_row("SELECT COUNT(*) FROM recovery_tombstones", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(markers, 0);
    }

    #[test]
    fn disable_reenable_and_empty_or_older_snapshots_cannot_restore_or_erase_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("ordering.db")).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        let epoch = db.recovery().epoch().unwrap();
        db.recovery()
            .save("ordered", epoch, 10, "latest text")
            .unwrap();
        assert!(
            !db.recovery()
                .save("ordered", epoch, 9, "older text")
                .unwrap()
        );
        assert!(!db.recovery().save("ordered", epoch, 11, "").unwrap());
        assert_eq!(
            db.recovery().text("ordered").unwrap().as_deref(),
            Some("latest text")
        );
        db.history()
            .set_retention(RetentionPolicy::Disabled, 12)
            .unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 13)
            .unwrap();
        assert!(
            !db.recovery()
                .save("ordered", epoch, 14, "queued before disable")
                .unwrap()
        );
        assert!(db.recovery().list().unwrap().is_empty());
    }

    #[test]
    fn ordinary_retention_purge_removes_recovery_without_reading_payload() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("purge.db")).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Hours24, 0)
            .unwrap();
        let epoch = db.recovery().epoch().unwrap();
        db.recovery()
            .save("expired", epoch, 0, "expired transcript")
            .unwrap();
        db.recovery()
            .save("recent", epoch, 100_000_000, "recent transcript")
            .unwrap();
        // Metadata enumeration and retention must work even for corrupt data.
        db.connection
            .execute("UPDATE interrupted_dictation SET protected_text=x'00'", [])
            .unwrap();
        assert_eq!(db.recovery().list().unwrap().len(), 2);
        db.history().purge_expired(100_000_000).unwrap();
        let rows = db.recovery().list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session, "recent");
    }
    #[test]
    fn encrypted_snapshot_replaces_text_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.db");
        let db = Persistence::open(&path).unwrap();
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 10)
            .unwrap();
        let epoch = db.recovery().epoch().unwrap();
        assert!(
            db.recovery()
                .save("session", epoch, 10, "private unfinished phrase")
                .unwrap()
        );
        assert!(
            db.recovery()
                .save("session", epoch, 11, "corrected owned words")
                .unwrap()
        );
        let blob: Vec<u8> = db
            .connection
            .query_row(
                "SELECT protected_text FROM interrupted_dictation",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!blob.windows(9).any(|w| w == b"corrected"));
        drop(db);
        let db = Persistence::open(&path).unwrap();
        assert_eq!(
            db.recovery().text("session").unwrap().as_deref(),
            Some("corrected owned words")
        );
        db.recovery().discard("session").unwrap();
        assert!(db.recovery().list().unwrap().is_empty());
    }
    #[test]
    fn privacy_clear_rejects_stale_writes_and_retention_removes_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("privacy.db")).unwrap();
        assert_eq!(db.recovery().eligibility_epoch().unwrap(), None);
        assert!(!db.recovery().save("off", 0, 0, "secret").unwrap());
        db.history()
            .set_retention(RetentionPolicy::Indefinite, 0)
            .unwrap();
        let epoch = db.recovery().epoch().unwrap();
        assert_eq!(db.recovery().eligibility_epoch().unwrap(), Some(epoch));
        db.recovery().save("old", epoch, 0, "secret").unwrap();
        db.history().clear().unwrap();
        assert!(db.recovery().list().unwrap().is_empty());
        assert!(!db.recovery().save("late", epoch, 1, "late secret").unwrap());
        let epoch = db.recovery().epoch().unwrap();
        db.recovery().save("old", epoch, 0, "secret").unwrap();
        db.history()
            .set_retention(RetentionPolicy::Hours24, 100_000_000)
            .unwrap();
        assert!(db.recovery().list().unwrap().is_empty());
        db.recovery()
            .save("new", db.recovery().epoch().unwrap(), 100_000_000, "secret")
            .unwrap();
        db.history()
            .set_retention(RetentionPolicy::Disabled, 100_000_001)
            .unwrap();
        assert!(db.recovery().list().unwrap().is_empty());
    }
}
