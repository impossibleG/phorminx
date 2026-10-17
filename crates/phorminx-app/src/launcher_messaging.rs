//! What the launcher needs to know about messaging: which main-panel key opens
//! the contacts panel, which contacts are promoted to their own main-panel
//! key, and which contacts sit on keys 1-9 of the contacts panel. Only names
//! leave this module; addresses and keys never reach the overlay.

use phorminx_persistence::Persistence;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LauncherMessaging {
    /// Main-panel key (3-9) that opens the contacts panel.
    pub launcher_slot: Option<u8>,
    /// (main-panel key, contact id, name) for promoted contacts.
    pub promoted: Vec<(u8, i64, String)>,
    /// (contacts-panel key, contact id, name).
    pub contacts: Vec<(u8, i64, String)>,
}

impl LauncherMessaging {
    /// Reads the launcher layout. Empty unless messaging is enabled.
    pub fn load(db: &Persistence) -> Self {
        let repo = db.messaging();
        if !repo.identity().ok().flatten().is_some_and(|i| i.enabled) {
            return Self::default();
        }
        let contacts = repo.contacts().unwrap_or_default();
        let available = || contacts.iter().filter(|c| !c.blocked);
        Self {
            launcher_slot: repo.launcher_slot().ok().flatten(),
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

    /// The messaging key, unless a saved HTTP action already uses it.
    pub fn effective_launcher_slot(&self, action_keys: &[u8]) -> Option<u8> {
        self.launcher_slot.filter(|key| !action_keys.contains(key))
    }

    /// Extra main-panel rows for keys that saved HTTP actions do not use.
    pub fn main_rows(&self, action_keys: &[u8]) -> Vec<(u8, String)> {
        let mut rows = Vec::new();
        if let Some(key) = self.effective_launcher_slot(action_keys) {
            rows.push((key, "Messages".to_owned()));
        }
        for (key, _, name) in &self.promoted {
            if !action_keys.contains(key) && Some(*key) != self.launcher_slot {
                rows.push((*key, format!("Message {name}")));
            }
        }
        rows
    }

    /// The promoted contact on a main-panel key.
    pub fn promoted_contact(&self, key: u8, action_keys: &[u8]) -> Option<i64> {
        if action_keys.contains(&key) || Some(key) == self.launcher_slot {
            return None;
        }
        self.promoted
            .iter()
            .find(|(slot, _, _)| *slot == key)
            .map(|(_, id, _)| *id)
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
    fn disabled_messaging_adds_nothing_to_the_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("p.db")).unwrap();
        contact(&db, 1, "Ana");
        db.messaging().set_launcher_slot(Some(5)).unwrap();
        assert_eq!(LauncherMessaging::load(&db), LauncherMessaging::default());
        enable(&db, false);
        assert_eq!(LauncherMessaging::load(&db), LauncherMessaging::default());
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
        let layout = LauncherMessaging::load(&db);
        assert_eq!(
            layout.panel_rows(),
            vec![(1, "Ana".to_owned()), (2, "Robert".to_owned())]
        );
        assert_eq!(layout.contact_for_key(2), Some(bob));
        assert_eq!(layout.contact_for_key(3), None);
        // A saved HTTP action on 8 wins over the promoted contact.
        let actions = [8];
        assert_eq!(
            layout.main_rows(&actions),
            vec![(5, "Messages".to_owned()), (7, "Message Ana".to_owned())]
        );
        assert_eq!(layout.promoted_contact(7, &actions), Some(ana));
        assert_eq!(layout.promoted_contact(8, &actions), None);
        assert_eq!(layout.effective_launcher_slot(&[5]), None);
        assert_eq!(
            layout.main_rows(&[5]),
            vec![
                (7, "Message Ana".to_owned()),
                (8, "Message Robert".to_owned())
            ]
        );
    }
}
