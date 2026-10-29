//! What the launcher needs to know about investigations. The main panel offers
//! **Investigations** on key 9, or the first free key 3-9 when a saved HTTP
//! action (or messaging) owns 9. It opens a second panel:
//!
//! - nothing active, "Pick one to start": 1-8 the most recently updated
//!   investigations (picking one makes it active and shows the capture
//!   panel), 9 New investigation;
//! - active, "Add to <title>": 1 Voice note, 2 Clipboard, 3 Image, 4 Link,
//!   5 Note, 8 Switch investigation, 9 Stop, 0 Open in Phorminx.
//!
//! Only titles leave this module; card contents never reach the overlay.

use phorminx_persistence::Persistence;

/// The key Investigations takes when it is free.
pub const PREFERRED_KEY: u8 = 9;
/// Investigations listed in the pick panel (keys 1-8).
const PICK_LIMIT: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LauncherInvestigations {
    /// (id, title), most recently updated first, at most eight.
    pub recent: Vec<(i64, String)>,
    pub active: Option<(i64, String)>,
}

/// Work the launcher hands to the shell that owns the investigation service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvestigationRequest {
    /// Send the clipboard to the active investigation.
    QuickCapture,
    /// Open the small capture window on `"note"`, `"link"`, `"image"` or
    /// `"new"` (name a new investigation).
    CaptureWindow(&'static str),
    /// File a routed launcher dictation as a voice note.
    VoiceNote { investigation_id: i64, text: String },
    /// Show this investigation in the main window.
    Open(i64),
    /// Capturing stopped; confirm it with the investigation's title.
    Stopped(String),
}

/// Which investigations panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvestigationPanel {
    Pick,
    Capture,
}

/// What a key in the investigations panel does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvestigationChoice {
    Pick(i64),
    New,
    Voice,
    Clipboard,
    Image,
    Link,
    Note,
    Switch,
    Stop,
    Open,
}

impl LauncherInvestigations {
    pub fn load(db: &Persistence) -> Self {
        let repo = db.investigations();
        let all = repo.list().unwrap_or_default();
        let active = repo.active().ok().flatten().and_then(|id| {
            all.iter()
                .find(|record| record.id == id)
                .map(|record| (record.id, record.title.clone()))
        });
        Self {
            recent: all
                .into_iter()
                .take(PICK_LIMIT)
                .map(|record| (record.id, record.title))
                .collect(),
            active,
        }
    }

    /// The main-panel key, around the keys actions and messaging already use.
    pub fn launcher_key(taken: &[u8]) -> Option<u8> {
        Some(PREFERRED_KEY)
            .filter(|key| !taken.contains(key))
            .or_else(|| (3..=9).find(|key| !taken.contains(key)))
    }

    /// The panel the Investigations key opens.
    pub fn first_panel(&self) -> InvestigationPanel {
        if self.active.is_some() {
            InvestigationPanel::Capture
        } else {
            InvestigationPanel::Pick
        }
    }

    pub fn title(&self, panel: InvestigationPanel) -> String {
        match (panel, &self.active) {
            (InvestigationPanel::Capture, Some((_, title))) => format!("Add to {title}"),
            _ => "Pick one to start".to_owned(),
        }
    }

    pub fn hint(&self, panel: InvestigationPanel) -> &'static str {
        match panel {
            InvestigationPanel::Pick => "Choose an investigation",
            InvestigationPanel::Capture => "Choose what to add",
        }
    }

    /// Rows in display order.
    pub fn rows(&self, panel: InvestigationPanel) -> Vec<(u8, String)> {
        match panel {
            InvestigationPanel::Pick => (1u8..)
                .zip(&self.recent)
                .map(|(key, (_, title))| (key, title.clone()))
                .chain([(9, "New investigation".to_owned())])
                .collect(),
            InvestigationPanel::Capture => [
                (1, "Voice note"),
                (2, "Clipboard"),
                (3, "Image"),
                (4, "Link"),
                (5, "Note"),
                (8, "Switch investigation"),
                (9, "Stop"),
                (0, "Open in Phorminx"),
            ]
            .into_iter()
            .map(|(key, label)| (key, label.to_owned()))
            .collect(),
        }
    }

    /// Keys the panel answers to, and those that keep it open because they
    /// lead to the other panel.
    pub fn keys(&self, panel: InvestigationPanel) -> (Vec<u8>, Vec<u8>) {
        let keys = self.rows(panel).into_iter().map(|(key, _)| key).collect();
        let stay = match panel {
            InvestigationPanel::Pick => (1u8..).take(self.recent.len()).collect(),
            InvestigationPanel::Capture => vec![8],
        };
        (keys, stay)
    }

    pub fn choice(&self, panel: InvestigationPanel, key: u8) -> Option<InvestigationChoice> {
        match panel {
            InvestigationPanel::Pick => match key {
                9 => Some(InvestigationChoice::New),
                1..=8 => self
                    .recent
                    .get(usize::from(key) - 1)
                    .map(|(id, _)| InvestigationChoice::Pick(*id)),
                _ => None,
            },
            InvestigationPanel::Capture => match key {
                1 => Some(InvestigationChoice::Voice),
                2 => Some(InvestigationChoice::Clipboard),
                3 => Some(InvestigationChoice::Image),
                4 => Some(InvestigationChoice::Link),
                5 => Some(InvestigationChoice::Note),
                8 => Some(InvestigationChoice::Switch),
                9 => Some(InvestigationChoice::Stop),
                0 => Some(InvestigationChoice::Open),
                _ => None,
            },
        }
    }

    /// Makes `id` the active investigation (`None` stops capturing).
    pub fn set_active(&mut self, db: &Persistence, id: Option<i64>) -> Result<(), String> {
        db.investigations()
            .set_active(id)
            .map_err(|_| "Choose an existing investigation.".to_owned())?;
        self.active = id.and_then(|id| {
            self.recent
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .cloned()
                .or_else(|| {
                    db.investigations()
                        .get(id)
                        .ok()
                        .flatten()
                        .map(|record| (record.id, record.title))
                })
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with(titles: &[&str]) -> (tempfile::TempDir, Persistence, Vec<i64>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Persistence::open(dir.path().join("p.db")).unwrap();
        let ids = titles
            .iter()
            .enumerate()
            .map(|(index, title)| db.investigations().create(title, index as i64 + 1).unwrap())
            .collect();
        (dir, db, ids)
    }

    #[test]
    fn takes_nine_unless_an_action_or_messaging_owns_it() {
        assert_eq!(LauncherInvestigations::launcher_key(&[]), Some(9));
        assert_eq!(LauncherInvestigations::launcher_key(&[3, 9]), Some(4));
        assert_eq!(
            LauncherInvestigations::launcher_key(&[3, 4, 5, 6, 7, 8, 9]),
            None
        );
    }

    #[test]
    fn without_an_active_investigation_the_panel_lists_recent_ones() {
        let titles: Vec<String> = (1..=10).map(|n| format!("Case {n}")).collect();
        let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
        let (_dir, db, ids) = db_with(&refs);
        let launcher = LauncherInvestigations::load(&db);
        let panel = launcher.first_panel();
        assert_eq!(panel, InvestigationPanel::Pick);
        assert_eq!(launcher.title(panel), "Pick one to start");
        let rows = launcher.rows(panel);
        // Most recently updated first, eight of them, then New on 9.
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0], (1, "Case 10".to_owned()));
        assert_eq!(rows[7], (8, "Case 3".to_owned()));
        assert_eq!(rows[8], (9, "New investigation".to_owned()));
        assert_eq!(
            launcher.choice(panel, 1),
            Some(InvestigationChoice::Pick(ids[9]))
        );
        assert_eq!(launcher.choice(panel, 9), Some(InvestigationChoice::New));
        assert_eq!(launcher.choice(panel, 0), None);
        let (keys, stay) = launcher.keys(panel);
        assert_eq!(keys, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(stay, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn an_empty_list_offers_only_new() {
        let (_dir, db, _) = db_with(&[]);
        let launcher = LauncherInvestigations::load(&db);
        let panel = launcher.first_panel();
        assert_eq!(
            launcher.rows(panel),
            vec![(9, "New investigation".to_owned())]
        );
        assert_eq!(launcher.choice(panel, 1), None);
        assert_eq!(launcher.keys(panel), (vec![9], vec![]));
    }

    #[test]
    fn picking_activates_and_the_capture_panel_follows() {
        let (_dir, db, ids) = db_with(&["Supplier review", "Leak"]);
        let mut launcher = LauncherInvestigations::load(&db);
        launcher.set_active(&db, Some(ids[0])).unwrap();
        assert_eq!(db.investigations().active().unwrap(), Some(ids[0]));
        let panel = launcher.first_panel();
        assert_eq!(panel, InvestigationPanel::Capture);
        assert_eq!(launcher.title(panel), "Add to Supplier review");
        let keys: Vec<u8> = launcher.rows(panel).iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, vec![1, 2, 3, 4, 5, 8, 9, 0]);
        assert_eq!(launcher.keys(panel).1, vec![8]);
        let choices: Vec<_> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 0]
            .into_iter()
            .map(|key| launcher.choice(panel, key))
            .collect();
        use InvestigationChoice::*;
        assert_eq!(
            choices,
            vec![
                Some(Voice),
                Some(Clipboard),
                Some(Image),
                Some(Link),
                Some(Note),
                None,
                None,
                Some(Switch),
                Some(Stop),
                Some(Open)
            ]
        );
        // Reloading sees the same active investigation.
        assert_eq!(
            LauncherInvestigations::load(&db).active,
            Some((ids[0], "Supplier review".to_owned()))
        );
        // Stop clears it everywhere.
        launcher.set_active(&db, None).unwrap();
        assert_eq!(db.investigations().active().unwrap(), None);
        assert_eq!(launcher.first_panel(), InvestigationPanel::Pick);
        assert!(launcher.set_active(&db, Some(999)).is_err());
    }
}
