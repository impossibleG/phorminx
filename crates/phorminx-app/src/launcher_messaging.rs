//! What the launcher needs to know about messaging. The main panel always
//! offers **Messages** on a free key 3-9 (the user's choice when it is free,
//! otherwise the first free key). With messaging on and contacts on keys 1-9 it
//! opens the contacts panel; otherwise it opens the Messages page so the user
//! can turn messaging on or add someone. Promoted contacts take their own key,
//! or the next free one. Only names leave this module; addresses and keys never
//! reach the overlay.

use phorminx_persistence::Persistence;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LauncherMessaging {
    pub enabled: bool,
    /// The main-panel key the user chose for Messages, if any.
    pub launcher_slot: Option<u8>,
    /// (preferred main-panel key, contact id, name) for promoted contacts.
    pub promoted: Vec<(u8, i64, String)>,
    /// (contacts-panel key, contact id, name).
    pub contacts: Vec<(u8, i64, String)>,
}

/// What the Messages key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessagesEntry {
    ContactsPanel,
    OpenPage,
}

/// Main-panel keys actually assigned, after saved HTTP actions took theirs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LauncherLayout {
    pub messages: Option<(u8, MessagesEntry)>,
    /// (key, contact id, name).
    pub promoted: Vec<(u8, i64, String)>,
}

impl LauncherMessaging {
    pub fn load(db: &Persistence) -> Self {
        let repo = db.messaging();
        let enabled = repo.identity().ok().flatten().is_some_and(|i| i.enabled);
        let launcher_slot = repo.launcher_slot().ok().flatten();
        if !enabled {
            return Self {
                launcher_slot,
                ..Self::default()
            };
        }
        let contacts = repo.contacts().unwrap_or_default();
        let available = || contacts.iter().filter(|c| !c.blocked);
        Self {
            enabled,
            launcher_slot,
            promoted: available()
                .filter_map(|c| {
                    c.main_slot
                        .map(|key| (key, c.id, c.shown_name().to_owned()))
                })
                .collect(),
            contacts: available()
                .filter_map(|c| {
                    c.contacts_slot
                        .map(|key| (key, c.id, c.shown_name().to_owned()))
                })
                .collect(),
        }
    }

    /// Assigns main-panel keys around the keys saved HTTP actions use.
    pub fn layout(&self, action_keys: &[u8]) -> LauncherLayout {
        let mut used: Vec<u8> = action_keys.to_vec();
        let mut take = |preferred: Option<u8>| {
            let key = preferred
                .filter(|k| (3..=9).contains(k) && !used.contains(k))
                .or_else(|| (3..=9).find(|k| !used.contains(k)))?;
            used.push(key);
            Some(key)
        };
        let entry = if self.enabled && !self.contacts.is_empty() {
            MessagesEntry::ContactsPanel
        } else {
            MessagesEntry::OpenPage
        };
        let messages = take(self.launcher_slot).map(|key| (key, entry));
        let promoted = self
            .promoted
            .iter()
            .filter_map(|(key, id, name)| take(Some(*key)).map(|k| (k, *id, name.clone())))
            .collect();
        LauncherLayout { messages, promoted }
    }

    /// The contact on a contacts-panel key.
    pub fn contact_for_key(&self, key: u8) -> Option<i64> {
        self.contacts
            .iter()
            .find(|(slot, _, _)| *slot == key)
            .map(|(_, id, _)| *id)
    }

    /// Rows for the contacts panel.
    pub fn panel_rows(&self) -> Vec<(u8, String)> {
        self.contacts
            .iter()
            .map(|(key, _, name)| (*key, name.clone()))
            .collect()
    }
}

impl LauncherLayout {
    /// Rows to add to the main panel.
    pub fn main_rows(&self) -> Vec<(u8, String)> {
        self.messages
            .iter()
            .map(|(key, _)| (*key, "Messages".to_owned()))
            .chain(
                self.promoted
                    .iter()
                    .map(|(key, _, name)| (*key, format!("Message {name}"))),
            )
            .collect()
    }

    /// The key the keyboard hook turns into the contacts panel.
    pub fn panel_key(&self) -> Option<u8> {
        self.messages
            .filter(|(_, entry)| *entry == MessagesEntry::ContactsPanel)
            .map(|(key, _)| key)
    }

    /// The key that opens the Messages page instead.
    pub fn page_key(&self) -> Option<u8> {
        self.messages
            .filter(|(_, entry)| *entry == MessagesEntry::OpenPage)
            .map(|(key, _)| key)
    }

    pub fn promoted_contact(&self, key: u8) -> Option<i64> {
        self.promoted
            .iter()
            .find(|(slot, _, _)| *slot == key)
            .map(|(_, id, _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phorminx_persistence::{MessagingIdentityRecord, NewMessagingContact};

    fn contact(db: &Persistence, key: u8, name: &str) -> i64 {
        db.messaging()
            .upsert_contact(
                &NewMessagingContact {
                    identity_key: &[key; 32],
                    contacts_address: "abc.onion",
                    display_name: name,
                    discovery_key: None,
                },
                1,
            )
            .unwrap()
    }

    fn enable(db: &Persistence, enabled: bool) {
        db.messaging()
            .create_identity(&MessagingIdentityRecord {
                display_name: "Me".into(),
                public_key: vec![1; 32],
                protected_secret: vec![2; 40],
                enabled,
                contacts_address: None,
                created_at_ms: 1,
            })
            .unwrap();
    }

    #[test]
    fn messages_is_offered_by_default_and_opens_the_page_when_off() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("p.db")).unwrap();
        // Never set up: Messages still shows, on the first free key.
        let layout = LauncherMessaging::load(&db).layout(&[3]);
        assert_eq!(layout.messages, Some((4, MessagesEntry::OpenPage)));
        assert_eq!(layout.main_rows(), vec![(4, "Messages".to_owned())]);
        assert_eq!((layout.page_key(), layout.panel_key()), (Some(4), None));
        // Turned off, with contacts: still the page, no promoted contacts.
        enable(&db, false);
        let ana = contact(&db, 1, "Ana");
        db.messaging().set_main_slot(ana, Some(6)).unwrap();
        let layout = LauncherMessaging::load(&db).layout(&[]);
        assert_eq!(layout.page_key(), Some(3));
        assert!(layout.promoted.is_empty());
    }

    #[test]
    fn enabled_without_contacts_opens_the_page() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("p.db")).unwrap();
        enable(&db, true);
        let layout = LauncherMessaging::load(&db).layout(&[]);
        assert_eq!(layout.messages, Some((3, MessagesEntry::OpenPage)));
    }

    #[test]
    fn layout_respects_saved_actions_and_blocked_contacts() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("p.db")).unwrap();
        enable(&db, true);
        let ana = contact(&db, 1, "Ana");
        let bob = contact(&db, 2, "Bob");
        let eve = contact(&db, 3, "Eve");
        let repo = db.messaging();
        repo.set_launcher_slot(Some(5)).unwrap();
        repo.set_main_slot(ana, Some(7)).unwrap();
        repo.set_main_slot(bob, Some(8)).unwrap();
        repo.rename_contact(bob, Some("Robert")).unwrap();
        repo.set_blocked(eve, true).unwrap();
        let messaging = LauncherMessaging::load(&db);
        assert_eq!(
            messaging.panel_rows(),
            vec![(1, "Ana".to_owned()), (2, "Robert".to_owned())]
        );
        assert_eq!(messaging.contact_for_key(2), Some(bob));
        assert_eq!(messaging.contact_for_key(3), None);
        let layout = messaging.layout(&[]);
        assert_eq!(layout.panel_key(), Some(5));
        assert_eq!(
            layout.main_rows(),
            vec![
                (5, "Messages".to_owned()),
                (7, "Message Ana".to_owned()),
                (8, "Message Robert".to_owned())
            ]
        );
        // Saved HTTP actions keep their keys; messaging moves to free ones.
        let layout = messaging.layout(&[5, 8]);
        assert_eq!(layout.panel_key(), Some(3));
        assert_eq!(layout.promoted_contact(7), Some(ana));
        assert_eq!(layout.promoted_contact(4), Some(bob));
        assert_eq!(layout.promoted_contact(8), None);
        // No free key left: nothing is shown rather than taking an action key.
        let layout = messaging.layout(&[3, 4, 5, 6, 7, 8, 9]);
        assert!(layout.main_rows().is_empty());
    }
}
