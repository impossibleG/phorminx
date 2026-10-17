//! Additive storage for peer-to-peer messaging: identity, invites, contacts and
//! the local message history. Nothing here is ever sent to a server; the
//! private identity key is stored only as caller-protected (DPAPI) bytes.
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{PersistenceError, Result};

/// Upper bound for a stored message body, matching the wire limit.
pub const MESSAGING_MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 64;
const MAX_ADDRESS_BYTES: usize = 128;
const MAX_BLOB_BYTES: usize = 4096;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS messaging_identity (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 display_name TEXT NOT NULL,
 public_key BLOB NOT NULL,
 protected_secret BLOB NOT NULL,
 enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
 contacts_address TEXT,
 created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messaging_invites (
 id TEXT PRIMARY KEY NOT NULL,
 secret_hash BLOB NOT NULL,
 address TEXT NOT NULL,
 created_at_ms INTEGER NOT NULL,
 expires_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messaging_contacts (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 identity_key BLOB NOT NULL UNIQUE,
 contacts_address TEXT NOT NULL,
 display_name TEXT NOT NULL,
 nickname TEXT,
 contacts_slot INTEGER UNIQUE CHECK(contacts_slot BETWEEN 1 AND 9),
 main_slot INTEGER UNIQUE CHECK(main_slot BETWEEN 3 AND 9),
 blocked INTEGER NOT NULL DEFAULT 0 CHECK(blocked IN (0,1)),
 discovery_key BLOB,
 added_at_ms INTEGER NOT NULL,
 last_seen_ms INTEGER
);
CREATE TABLE IF NOT EXISTS messaging_messages (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 contact_id INTEGER NOT NULL REFERENCES messaging_contacts(id) ON DELETE CASCADE,
 message_uuid TEXT NOT NULL,
 direction TEXT NOT NULL CHECK(direction IN ('in','out')),
 text TEXT NOT NULL,
 sent_at_ms INTEGER NOT NULL,
 received_at_ms INTEGER,
 state TEXT NOT NULL CHECK(state IN ('sending','waiting','delivered','received')),
 unread INTEGER NOT NULL DEFAULT 0 CHECK(unread IN (0,1)),
 attempts INTEGER NOT NULL DEFAULT 0,
 UNIQUE(contact_id, direction, message_uuid)
);
CREATE INDEX IF NOT EXISTS messaging_messages_contact_idx ON messaging_messages(contact_id, id);
CREATE INDEX IF NOT EXISTS messaging_messages_outbox_idx ON messaging_messages(state) WHERE state IN ('sending','waiting');
"#;

#[derive(Clone, PartialEq, Eq)]
pub struct MessagingIdentityRecord {
    pub display_name: String,
    pub public_key: Vec<u8>,
    /// Caller-protected private key bytes (DPAPI ciphertext in the app).
    pub protected_secret: Vec<u8>,
    pub enabled: bool,
    pub contacts_address: Option<String>,
    pub created_at_ms: i64,
}

impl std::fmt::Debug for MessagingIdentityRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagingIdentityRecord")
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagingInviteRecord {
    pub id: String,
    pub secret_hash: Vec<u8>,
    pub address: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagingContactRecord {
    pub id: i64,
    pub identity_key: Vec<u8>,
    pub contacts_address: String,
    pub display_name: String,
    pub nickname: Option<String>,
    pub contacts_slot: Option<u8>,
    pub main_slot: Option<u8>,
    pub blocked: bool,
    pub discovery_key: Option<Vec<u8>>,
    pub added_at_ms: i64,
    pub last_seen_ms: Option<i64>,
}

impl MessagingContactRecord {
    /// The name the UI shows: the local nickname, else the peer's display name.
    pub fn shown_name(&self) -> &str {
        self.nickname.as_deref().unwrap_or(&self.display_name)
    }
}

/// Data needed to create or refresh a contact after an invite exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMessagingContact<'a> {
    pub identity_key: &'a [u8],
    pub contacts_address: &'a str,
    pub display_name: &'a str,
    pub discovery_key: Option<&'a [u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessagingDirection {
    Incoming,
    Outgoing,
}

impl MessagingDirection {
    fn as_db(self) -> &'static str {
        match self {
            Self::Incoming => "in",
            Self::Outgoing => "out",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessagingMessageState {
    /// Outgoing, not yet attempted or an attempt is in flight.
    Sending,
    /// Outgoing, the last attempt failed; it will be retried.
    Waiting,
    /// Outgoing and acknowledged by the recipient.
    Delivered,
    /// Incoming.
    Received,
}

impl MessagingMessageState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Waiting => "waiting",
            Self::Delivered => "delivered",
            Self::Received => "received",
        }
    }
    fn from_db(s: &str) -> rusqlite::Result<Self> {
        match s {
            "sending" => Ok(Self::Sending),
            "waiting" => Ok(Self::Waiting),
            "delivered" => Ok(Self::Delivered),
            "received" => Ok(Self::Received),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagingMessageRecord {
    pub id: i64,
    pub contact_id: i64,
    pub message_uuid: String,
    pub direction: MessagingDirection,
    pub text: String,
    pub sent_at_ms: i64,
    pub received_at_ms: Option<i64>,
    pub state: MessagingMessageState,
    pub unread: bool,
    pub attempts: u32,
}

pub struct MessagingRepository<'a> {
    connection: &'a Connection,
}

impl<'a> MessagingRepository<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    pub fn identity(&self) -> Result<Option<MessagingIdentityRecord>> {
        Ok(self
            .connection
            .query_row(
                "SELECT display_name,public_key,protected_secret,enabled,contacts_address,created_at_ms \
                 FROM messaging_identity WHERE singleton=1",
                [],
                |r| {
                    Ok(MessagingIdentityRecord {
                        display_name: r.get(0)?,
                        public_key: r.get(1)?,
                        protected_secret: r.get(2)?,
                        enabled: r.get(3)?,
                        contacts_address: r.get(4)?,
                        created_at_ms: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Creates the identity once. An existing identity is never overwritten.
    pub fn create_identity(&self, record: &MessagingIdentityRecord) -> Result<()> {
        name("display_name", &record.display_name)?;
        blob("public_key", &record.public_key)?;
        blob("protected_secret", &record.protected_secret)?;
        let inserted = self.connection.execute(
            "INSERT OR IGNORE INTO messaging_identity(singleton,display_name,public_key,protected_secret,enabled,contacts_address,created_at_ms) \
             VALUES (1,?1,?2,?3,?4,?5,?6)",
            params![
                record.display_name,
                record.public_key,
                record.protected_secret,
                record.enabled,
                record.contacts_address,
                record.created_at_ms
            ],
        )?;
        if inserted == 0 {
            return Err(invalid("messaging_identity", "identity already exists"));
        }
        Ok(())
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.connection.execute(
            "UPDATE messaging_identity SET enabled=?1 WHERE singleton=1",
            [enabled],
        )?;
        Ok(())
    }

    pub fn set_display_name(&self, display_name: &str) -> Result<()> {
        name("display_name", display_name)?;
        self.connection.execute(
            "UPDATE messaging_identity SET display_name=?1 WHERE singleton=1",
            [display_name],
        )?;
        Ok(())
    }

    pub fn set_contacts_address(&self, address: &str) -> Result<()> {
        address_check(address)?;
        self.connection.execute(
            "UPDATE messaging_identity SET contacts_address=?1 WHERE singleton=1",
            [address],
        )?;
        Ok(())
    }

    /// The main-launcher key (3-9) that opens the messaging contacts panel.
    pub fn launcher_slot(&self) -> Result<Option<u8>> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM persistence_settings WHERE key='messaging_launcher_slot'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|s| (3..=9).contains(s)))
    }

    /// Sets or clears the messaging launcher key. A contact promoted to the
    /// same key loses it.
    pub fn set_launcher_slot(&self, slot: Option<u8>) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        match slot {
            Some(slot) if (3..=9).contains(&slot) => {
                tx.execute(
                    "INSERT INTO persistence_settings(key,value) VALUES ('messaging_launcher_slot',?1) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    [slot.to_string()],
                )?;
                tx.execute(
                    "UPDATE messaging_contacts SET main_slot=NULL WHERE main_slot=?1",
                    [slot],
                )?;
            }
            Some(_) => return Err(invalid("messaging_launcher_slot", "must be 3-9")),
            None => {
                tx.execute(
                    "DELETE FROM persistence_settings WHERE key='messaging_launcher_slot'",
                    [],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_invite(&self, invite: &MessagingInviteRecord) -> Result<()> {
        address_check(&invite.address)?;
        blob("secret_hash", &invite.secret_hash)?;
        if invite.id.is_empty() || invite.id.len() > 64 {
            return Err(invalid("invite_id", "invalid length"));
        }
        self.connection.execute(
            "INSERT INTO messaging_invites(id,secret_hash,address,created_at_ms,expires_at_ms) VALUES (?1,?2,?3,?4,?5)",
            params![
                invite.id,
                invite.secret_hash,
                invite.address,
                invite.created_at_ms,
                invite.expires_at_ms
            ],
        )?;
        Ok(())
    }

    pub fn invites(&self) -> Result<Vec<MessagingInviteRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id,secret_hash,address,created_at_ms,expires_at_ms FROM messaging_invites ORDER BY created_at_ms, id",
        )?;
        Ok(statement
            .query_map([], |r| {
                Ok(MessagingInviteRecord {
                    id: r.get(0)?,
                    secret_hash: r.get(1)?,
                    address: r.get(2)?,
                    created_at_ms: r.get(3)?,
                    expires_at_ms: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Removes an invite. Returns whether it still existed, so exactly one
    /// redeemer can win a race for the same invite.
    pub fn delete_invite(&self, id: &str) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM messaging_invites WHERE id=?1", [id])?
            > 0)
    }

    /// Inserts a contact, or refreshes the address, display name and discovery
    /// key of a known identity (for example after a second invite). Local
    /// nickname, slots and block state are kept.
    pub fn upsert_contact(&self, contact: &NewMessagingContact<'_>, now_ms: i64) -> Result<i64> {
        blob("identity_key", contact.identity_key)?;
        address_check(contact.contacts_address)?;
        name("display_name", contact.display_name)?;
        if let Some(key) = contact.discovery_key {
            blob("discovery_key", key)?;
        }
        // A new contact gets the lowest free contacts-panel key, if any.
        Ok(self.connection.query_row(
            "INSERT INTO messaging_contacts(identity_key,contacts_address,display_name,discovery_key,added_at_ms,last_seen_ms,contacts_slot) \
             VALUES (?1,?2,?3,?4,?5,?5,(WITH RECURSIVE keys(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM keys WHERE n<9) \
             SELECT MIN(n) FROM keys WHERE n NOT IN (SELECT contacts_slot FROM messaging_contacts WHERE contacts_slot IS NOT NULL))) \
             ON CONFLICT(identity_key) DO UPDATE SET contacts_address=excluded.contacts_address, \
             display_name=excluded.display_name, discovery_key=COALESCE(excluded.discovery_key, discovery_key), \
             last_seen_ms=excluded.last_seen_ms \
             RETURNING id",
            params![
                contact.identity_key,
                contact.contacts_address,
                contact.display_name,
                contact.discovery_key,
                now_ms
            ],
            |r| r.get(0),
        )?)
    }

    pub fn contacts(&self) -> Result<Vec<MessagingContactRecord>> {
        let mut statement = self.connection.prepare(&format!(
            "{CONTACT_COLUMNS} ORDER BY COALESCE(nickname, display_name) COLLATE NOCASE, id"
        ))?;
        Ok(statement
            .query_map([], contact_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn contact(&self, id: i64) -> Result<Option<MessagingContactRecord>> {
        Ok(self
            .connection
            .query_row(&format!("{CONTACT_COLUMNS} WHERE id=?1"), [id], contact_row)
            .optional()?)
    }

    pub fn contact_by_key(&self, identity_key: &[u8]) -> Result<Option<MessagingContactRecord>> {
        Ok(self
            .connection
            .query_row(
                &format!("{CONTACT_COLUMNS} WHERE identity_key=?1"),
                [identity_key],
                contact_row,
            )
            .optional()?)
    }

    pub fn rename_contact(&self, id: i64, nickname: Option<&str>) -> Result<()> {
        let nickname = nickname.map(str::trim).filter(|n| !n.is_empty());
        if let Some(nickname) = nickname {
            name("nickname", nickname)?;
        }
        self.changed(self.connection.execute(
            "UPDATE messaging_contacts SET nickname=?2 WHERE id=?1",
            params![id, nickname],
        )?)
    }

    pub fn set_display_name_for(&self, id: i64, display_name: &str) -> Result<()> {
        name("display_name", display_name)?;
        self.connection.execute(
            "UPDATE messaging_contacts SET display_name=?2 WHERE id=?1",
            params![id, display_name],
        )?;
        Ok(())
    }

    pub fn set_blocked(&self, id: i64, blocked: bool) -> Result<()> {
        self.changed(self.connection.execute(
            "UPDATE messaging_contacts SET blocked=?2, main_slot=CASE WHEN ?2 THEN NULL ELSE main_slot END WHERE id=?1",
            params![id, blocked],
        )?)
    }

    /// Assigns a contacts-panel key (1-9). Another contact holding that key
    /// loses it. `None` clears the key.
    pub fn set_contacts_slot(&self, id: i64, slot: Option<u8>) -> Result<()> {
        if slot.is_some_and(|s| !(1..=9).contains(&s)) {
            return Err(invalid("contacts_slot", "must be 1-9"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if let Some(slot) = slot {
            tx.execute(
                "UPDATE messaging_contacts SET contacts_slot=NULL WHERE contacts_slot=?1 AND id<>?2",
                params![slot, id],
            )?;
        }
        let changed = tx.execute(
            "UPDATE messaging_contacts SET contacts_slot=?2 WHERE id=?1",
            params![id, slot],
        )?;
        tx.commit()?;
        self.changed(changed)
    }

    /// Promotes a contact to a main-panel key (3-9) or clears it. The caller
    /// checks that the key is not used by another launcher action.
    pub fn set_main_slot(&self, id: i64, slot: Option<u8>) -> Result<()> {
        if slot.is_some_and(|s| !(3..=9).contains(&s)) {
            return Err(invalid("main_slot", "must be 3-9"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if let Some(slot) = slot {
            tx.execute(
                "UPDATE messaging_contacts SET main_slot=NULL WHERE main_slot=?1 AND id<>?2",
                params![slot, id],
            )?;
        }
        let changed = tx.execute(
            "UPDATE messaging_contacts SET main_slot=?2 WHERE id=?1",
            params![id, slot],
        )?;
        tx.commit()?;
        self.changed(changed)
    }

    pub fn touch_contact(&self, id: i64, now_ms: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE messaging_contacts SET last_seen_ms=?2 WHERE id=?1",
            params![id, now_ms],
        )?;
        Ok(())
    }

    /// Deletes a contact and, through the foreign key, its whole history.
    pub fn remove_contact(&self, id: i64) -> Result<()> {
        self.changed(
            self.connection
                .execute("DELETE FROM messaging_contacts WHERE id=?1", [id])?,
        )
    }

    pub fn insert_outgoing(
        &self,
        contact_id: i64,
        message_uuid: &str,
        text: &str,
        sent_at_ms: i64,
    ) -> Result<i64> {
        message_text(text)?;
        uuid_check(message_uuid)?;
        Ok(self.connection.query_row(
            "INSERT INTO messaging_messages(contact_id,message_uuid,direction,text,sent_at_ms,state) \
             VALUES (?1,?2,'out',?3,?4,'sending') RETURNING id",
            params![contact_id, message_uuid, text, sent_at_ms],
            |r| r.get(0),
        )?)
    }

    /// Stores an incoming message. Returns `None` for a duplicate delivery of
    /// the same message id from the same contact, so the caller can still ack.
    pub fn insert_incoming(
        &self,
        contact_id: i64,
        message_uuid: &str,
        text: &str,
        sent_at_ms: i64,
        received_at_ms: i64,
    ) -> Result<Option<i64>> {
        message_text(text)?;
        uuid_check(message_uuid)?;
        Ok(self
            .connection
            .query_row(
                "INSERT INTO messaging_messages(contact_id,message_uuid,direction,text,sent_at_ms,received_at_ms,state,unread) \
                 VALUES (?1,?2,'in',?3,?4,?5,'received',1) \
                 ON CONFLICT(contact_id,direction,message_uuid) DO NOTHING RETURNING id",
                params![contact_id, message_uuid, text, sent_at_ms, received_at_ms],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Marks an outgoing message as delivered. Returns its row id if it
    /// changed state (a repeated ack returns `None`).
    pub fn mark_delivered(
        &self,
        contact_id: i64,
        message_uuid: &str,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "UPDATE messaging_messages SET state='delivered', received_at_ms=?3 \
                 WHERE contact_id=?1 AND message_uuid=?2 AND direction='out' AND state<>'delivered' RETURNING id",
                params![contact_id, message_uuid, now_ms],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Records a failed attempt for every undelivered outgoing message of a contact.
    pub fn mark_waiting(&self, contact_id: i64) -> Result<Vec<i64>> {
        let mut statement = self.connection.prepare(
            "UPDATE messaging_messages SET state='waiting', attempts=attempts+1 \
             WHERE contact_id=?1 AND direction='out' AND state IN ('sending','waiting') RETURNING id",
        )?;
        Ok(statement
            .query_map([contact_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Moves a waiting message back to sending when a new attempt starts.
    pub fn mark_sending(&self, id: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE messaging_messages SET state='sending' WHERE id=?1 AND direction='out' AND state='waiting'",
            [id],
        )? > 0)
    }

    /// Undelivered outgoing messages, oldest first.
    pub fn outbox(&self, contact_id: i64, limit: usize) -> Result<Vec<MessagingMessageRecord>> {
        let mut statement = self.connection.prepare(&format!(
            "{MESSAGE_COLUMNS} WHERE contact_id=?1 AND direction='out' AND state IN ('sending','waiting') ORDER BY id LIMIT ?2"
        ))?;
        Ok(statement
            .query_map(params![contact_id, limit as i64], message_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Contacts with undelivered outgoing messages.
    pub fn contacts_with_outbox(&self) -> Result<Vec<i64>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT contact_id FROM messaging_messages WHERE direction='out' AND state IN ('sending','waiting') ORDER BY contact_id",
        )?;
        Ok(statement
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The newest `limit` messages of a conversation, oldest first.
    pub fn conversation(
        &self,
        contact_id: i64,
        limit: usize,
    ) -> Result<Vec<MessagingMessageRecord>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT * FROM ({MESSAGE_COLUMNS} WHERE contact_id=?1 ORDER BY id DESC LIMIT ?2) ORDER BY id"
        ))?;
        Ok(statement
            .query_map(params![contact_id, limit.min(1000) as i64], message_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn message(&self, id: i64) -> Result<Option<MessagingMessageRecord>> {
        Ok(self
            .connection
            .query_row(&format!("{MESSAGE_COLUMNS} WHERE id=?1"), [id], message_row)
            .optional()?)
    }

    pub fn mark_read(&self, contact_id: i64) -> Result<usize> {
        Ok(self.connection.execute(
            "UPDATE messaging_messages SET unread=0 WHERE contact_id=?1 AND unread=1",
            [contact_id],
        )?)
    }

    /// Unread incoming message counts per contact (contacts with none are omitted).
    pub fn unread_counts(&self) -> Result<Vec<(i64, u32)>> {
        let mut statement = self.connection.prepare(
            "SELECT contact_id, COUNT(*) FROM messaging_messages WHERE unread=1 GROUP BY contact_id ORDER BY contact_id",
        )?;
        Ok(statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Deletes a conversation's history, keeping the contact. Undelivered
    /// outgoing messages are deleted too, so nothing is sent afterwards.
    pub fn delete_history(&self, contact_id: i64) -> Result<usize> {
        Ok(self.connection.execute(
            "DELETE FROM messaging_messages WHERE contact_id=?1",
            [contact_id],
        )?)
    }

    fn changed(&self, rows: usize) -> Result<()> {
        if rows == 0 {
            Err(invalid("messaging_contact", "unknown contact"))
        } else {
            Ok(())
        }
    }
}

const CONTACT_COLUMNS: &str = "SELECT id,identity_key,contacts_address,display_name,nickname,contacts_slot,main_slot,blocked,discovery_key,added_at_ms,last_seen_ms FROM messaging_contacts";
const MESSAGE_COLUMNS: &str = "SELECT id,contact_id,message_uuid,direction,text,sent_at_ms,received_at_ms,state,unread,attempts FROM messaging_messages";

fn contact_row(r: &Row<'_>) -> rusqlite::Result<MessagingContactRecord> {
    Ok(MessagingContactRecord {
        id: r.get(0)?,
        identity_key: r.get(1)?,
        contacts_address: r.get(2)?,
        display_name: r.get(3)?,
        nickname: r.get(4)?,
        contacts_slot: r.get(5)?,
        main_slot: r.get(6)?,
        blocked: r.get(7)?,
        discovery_key: r.get(8)?,
        added_at_ms: r.get(9)?,
        last_seen_ms: r.get(10)?,
    })
}

fn message_row(r: &Row<'_>) -> rusqlite::Result<MessagingMessageRecord> {
    let direction: String = r.get(3)?;
    let state: String = r.get(7)?;
    Ok(MessagingMessageRecord {
        id: r.get(0)?,
        contact_id: r.get(1)?,
        message_uuid: r.get(2)?,
        direction: if direction == MessagingDirection::Incoming.as_db() {
            MessagingDirection::Incoming
        } else {
            MessagingDirection::Outgoing
        },
        text: r.get(4)?,
        sent_at_ms: r.get(5)?,
        received_at_ms: r.get(6)?,
        state: MessagingMessageState::from_db(&state)?,
        unread: r.get(8)?,
        attempts: r.get(9)?,
    })
}

fn invalid(field: &'static str, reason: &'static str) -> PersistenceError {
    PersistenceError::Validation { field, reason }
}

fn name(field: &'static str, value: &str) -> Result<()> {
    let count = value.chars().count();
    if count == 0 || count > MAX_NAME_CHARS || value.chars().any(char::is_control) {
        return Err(invalid(field, "must be 1-64 printable characters"));
    }
    Ok(())
}

fn blob(field: &'static str, value: &[u8]) -> Result<()> {
    if value.is_empty() || value.len() > MAX_BLOB_BYTES {
        return Err(invalid(field, "invalid length"));
    }
    Ok(())
}

fn address_check(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ADDRESS_BYTES
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(invalid("address", "invalid onion address"));
    }
    Ok(())
}

fn uuid_check(value: &str) -> Result<()> {
    if value.len() != 36 || !value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(invalid("message_uuid", "invalid id"));
    }
    Ok(())
}

fn message_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(invalid("message_text", "empty message"));
    }
    if text.len() > MESSAGING_MAX_TEXT_BYTES {
        return Err(PersistenceError::TextLimitExceeded {
            field: "message_text",
            max_bytes: MESSAGING_MAX_TEXT_BYTES,
            actual_bytes: text.len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Persistence;

    const UUID_A: &str = "00000000-0000-4000-8000-000000000001";
    const UUID_B: &str = "00000000-0000-4000-8000-000000000002";

    fn open() -> (tempfile::TempDir, Persistence) {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("p.db")).unwrap();
        (dir, db)
    }

    fn contact(db: &Persistence, key: u8) -> i64 {
        db.messaging()
            .upsert_contact(
                &NewMessagingContact {
                    identity_key: &[key; 32],
                    contacts_address: "abc.onion",
                    display_name: "Ana",
                    discovery_key: None,
                },
                10,
            )
            .unwrap()
    }

    #[test]
    fn identity_is_created_once() {
        let (_d, db) = open();
        let repo = db.messaging();
        assert!(repo.identity().unwrap().is_none());
        let record = MessagingIdentityRecord {
            display_name: "mau".into(),
            public_key: vec![1; 32],
            protected_secret: vec![2; 64],
            enabled: true,
            contacts_address: None,
            created_at_ms: 5,
        };
        repo.create_identity(&record).unwrap();
        assert!(repo.create_identity(&record).is_err());
        repo.set_enabled(false).unwrap();
        repo.set_contacts_address("xyz.onion").unwrap();
        let stored = repo.identity().unwrap().unwrap();
        assert!(!stored.enabled);
        assert_eq!(stored.contacts_address.as_deref(), Some("xyz.onion"));
        assert!(!format!("{stored:?}").contains("protected"));
    }

    #[test]
    fn upsert_keeps_local_choices() {
        let (_d, db) = open();
        let repo = db.messaging();
        let id = contact(&db, 7);
        repo.rename_contact(id, Some("Mom")).unwrap();
        repo.set_contacts_slot(id, Some(2)).unwrap();
        repo.set_main_slot(id, Some(4)).unwrap();
        let again = repo
            .upsert_contact(
                &NewMessagingContact {
                    identity_key: &[7; 32],
                    contacts_address: "new.onion",
                    display_name: "Ana B",
                    discovery_key: Some(&[9; 32]),
                },
                20,
            )
            .unwrap();
        assert_eq!(again, id);
        let c = repo.contact(id).unwrap().unwrap();
        assert_eq!(c.shown_name(), "Mom");
        assert_eq!(c.contacts_address, "new.onion");
        assert_eq!((c.contacts_slot, c.main_slot), (Some(2), Some(4)));
        assert_eq!(c.discovery_key, Some(vec![9; 32]));
        assert!(repo.set_main_slot(id, Some(2)).is_err());
        assert!(repo.set_contacts_slot(id, Some(10)).is_err());
    }

    #[test]
    fn new_contacts_take_the_lowest_free_key() {
        let (_d, db) = open();
        let repo = db.messaging();
        let a = contact(&db, 1);
        let b = contact(&db, 2);
        assert_eq!(repo.contact(a).unwrap().unwrap().contacts_slot, Some(1));
        assert_eq!(repo.contact(b).unwrap().unwrap().contacts_slot, Some(2));
        repo.set_contacts_slot(a, None).unwrap();
        let c = contact(&db, 3);
        assert_eq!(repo.contact(c).unwrap().unwrap().contacts_slot, Some(1));
        // Refreshing a known contact keeps its key.
        assert_eq!(contact(&db, 2), b);
        assert_eq!(repo.contact(b).unwrap().unwrap().contacts_slot, Some(2));
        for key in 10..20 {
            contact(&db, key);
        }
        let without_key = repo
            .contacts()
            .unwrap()
            .iter()
            .filter(|c| c.contacts_slot.is_none())
            .count();
        assert_eq!(without_key, 4);
    }

    #[test]
    fn launcher_slot_roundtrip_clears_conflicting_promotion() {
        let (_d, db) = open();
        let repo = db.messaging();
        assert_eq!(repo.launcher_slot().unwrap(), None);
        let id = contact(&db, 1);
        repo.set_main_slot(id, Some(6)).unwrap();
        repo.set_launcher_slot(Some(6)).unwrap();
        assert_eq!(repo.launcher_slot().unwrap(), Some(6));
        assert_eq!(repo.contact(id).unwrap().unwrap().main_slot, None);
        assert!(repo.set_launcher_slot(Some(2)).is_err());
        repo.set_launcher_slot(None).unwrap();
        assert_eq!(repo.launcher_slot().unwrap(), None);
    }

    #[test]
    fn slots_move_between_contacts() {
        let (_d, db) = open();
        let repo = db.messaging();
        let a = contact(&db, 1);
        let b = contact(&db, 2);
        repo.set_contacts_slot(a, Some(5)).unwrap();
        repo.set_contacts_slot(b, Some(5)).unwrap();
        assert_eq!(repo.contact(a).unwrap().unwrap().contacts_slot, None);
        assert_eq!(repo.contact(b).unwrap().unwrap().contacts_slot, Some(5));
        repo.set_main_slot(a, Some(5)).unwrap();
        repo.set_blocked(a, true).unwrap();
        let blocked = repo.contact(a).unwrap().unwrap();
        assert!(blocked.blocked);
        assert_eq!(blocked.main_slot, None);
    }

    #[test]
    fn incoming_dedupes_and_counts_unread() {
        let (_d, db) = open();
        let repo = db.messaging();
        let id = contact(&db, 3);
        assert!(
            repo.insert_incoming(id, UUID_A, "hi", 1, 2)
                .unwrap()
                .is_some()
        );
        assert!(
            repo.insert_incoming(id, UUID_A, "hi", 1, 3)
                .unwrap()
                .is_none()
        );
        assert_eq!(repo.unread_counts().unwrap(), vec![(id, 1)]);
        assert_eq!(repo.mark_read(id).unwrap(), 1);
        assert!(repo.unread_counts().unwrap().is_empty());
        assert!(repo.insert_incoming(id, "bad", "hi", 1, 2).is_err());
        assert!(repo.insert_incoming(id, UUID_B, "  ", 1, 2).is_err());
    }

    #[test]
    fn outbox_lifecycle() {
        let (_d, db) = open();
        let repo = db.messaging();
        let id = contact(&db, 4);
        repo.insert_outgoing(id, UUID_A, "one", 1).unwrap();
        repo.insert_outgoing(id, UUID_B, "two", 2).unwrap();
        assert_eq!(repo.contacts_with_outbox().unwrap(), vec![id]);
        assert_eq!(repo.mark_waiting(id).unwrap().len(), 2);
        let outbox = repo.outbox(id, 10).unwrap();
        assert_eq!(outbox[0].state, MessagingMessageState::Waiting);
        assert_eq!(outbox[0].attempts, 1);
        assert!(repo.mark_delivered(id, UUID_A, 9).unwrap().is_some());
        assert!(repo.mark_delivered(id, UUID_A, 9).unwrap().is_none());
        assert_eq!(repo.outbox(id, 10).unwrap().len(), 1);
        let conversation = repo.conversation(id, 10).unwrap();
        assert_eq!(conversation[0].text, "one");
        assert_eq!(conversation[0].state, MessagingMessageState::Delivered);
        assert_eq!(repo.delete_history(id).unwrap(), 2);
        assert!(repo.contacts_with_outbox().unwrap().is_empty());
    }

    #[test]
    fn remove_contact_cascades() {
        let (_d, db) = open();
        let repo = db.messaging();
        let id = contact(&db, 5);
        repo.insert_outgoing(id, UUID_A, "one", 1).unwrap();
        repo.remove_contact(id).unwrap();
        assert!(repo.contact(id).unwrap().is_none());
        assert!(repo.conversation(id, 10).unwrap().is_empty());
        assert!(repo.remove_contact(id).is_err());
    }

    #[test]
    fn invites_are_single_use() {
        let (_d, db) = open();
        let repo = db.messaging();
        repo.insert_invite(&MessagingInviteRecord {
            id: "a1".into(),
            secret_hash: vec![1; 32],
            address: "inv.onion".into(),
            created_at_ms: 1,
            expires_at_ms: 2,
        })
        .unwrap();
        assert_eq!(repo.invites().unwrap().len(), 1);
        assert!(repo.delete_invite("a1").unwrap());
        assert!(!repo.delete_invite("a1").unwrap());
    }

    #[test]
    fn rejects_oversized_text() {
        let (_d, db) = open();
        let id = contact(&db, 6);
        let big = "x".repeat(MESSAGING_MAX_TEXT_BYTES + 1);
        assert!(db.messaging().insert_outgoing(id, UUID_A, &big, 1).is_err());
    }
}
