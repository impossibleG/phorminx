//! The small "message arrived" panel: which sender to name, how many new
//! messages it covers and what (if anything) of the text to show. Several
//! messages collapse into one panel instead of stacking notifications.

use serde::Serialize;

/// Characters of message text shown in the panel.
pub const PREVIEW_CHARS: usize = 60;

/// How arrivals are announced (`messaging_arrival_notice` setting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalNotice {
    Preview,
    NameOnly,
    Off,
}

impl ArrivalNotice {
    pub fn from_setting(value: &str) -> Self {
        match value {
            "name" => Self::NameOnly,
            "off" => Self::Off,
            _ => Self::Preview,
        }
    }
}

/// What the panel shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrivalView {
    /// The contact a click opens (the latest sender).
    pub contact_id: i64,
    /// "Ana", or "Ana + 1 more" when several people wrote.
    pub title: String,
    /// "3 new messages", or the latest text cut to one short line.
    pub detail: String,
    pub count: u32,
}

/// Messages that arrived since the panel was last dismissed.
#[derive(Default)]
pub struct Arrivals {
    /// (contact id, name) in arrival order, one entry per contact.
    senders: Vec<(i64, String)>,
    latest_text: String,
    count: u32,
}

impl Arrivals {
    pub fn push(&mut self, contact_id: i64, name: &str, text: &str) {
        self.senders.retain(|(id, _)| *id != contact_id);
        self.senders.push((contact_id, name.to_owned()));
        self.latest_text = text.to_owned();
        self.count += 1;
    }

    /// Forgets messages from a conversation the user is now reading.
    pub fn clear_contact(&mut self, contact_id: i64) {
        if self.senders.iter().any(|(id, _)| *id == contact_id) {
            self.senders.retain(|(id, _)| *id != contact_id);
            if self.senders.is_empty() {
                self.clear();
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn view(&self, notice: ArrivalNotice) -> Option<ArrivalView> {
        if notice == ArrivalNotice::Off {
            return None;
        }
        let (contact_id, name) = self.senders.last()?;
        let others = self.senders.len() - 1;
        let title = if others == 0 {
            name.clone()
        } else {
            format!("{name} + {others} more")
        };
        let detail = if notice == ArrivalNotice::Preview && self.count == 1 {
            preview(&self.latest_text)
        } else if self.count == 1 {
            "New message".to_owned()
        } else {
            format!("{} new messages", self.count)
        };
        Some(ArrivalView {
            contact_id: *contact_id,
            title,
            detail,
            count: self.count,
        })
    }
}

/// The first line of a message, cut to `PREVIEW_CHARS` characters.
pub fn preview(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(PREVIEW_CHARS).collect();
    if line.chars().count() > PREVIEW_CHARS || text.trim().lines().count() > 1 {
        out = out.trim_end().to_owned();
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_message_shows_name_and_a_short_preview() {
        let mut arrivals = Arrivals::default();
        arrivals.push(1, "Ana", "Oi! Chego às 8, pode ser?");
        let view = arrivals.view(ArrivalNotice::Preview).unwrap();
        assert_eq!(view.title, "Ana");
        assert_eq!(view.detail, "Oi! Chego às 8, pode ser?");
        assert_eq!((view.contact_id, view.count), (1, 1));
    }

    #[test]
    fn name_only_and_off_hide_the_text() {
        let mut arrivals = Arrivals::default();
        arrivals.push(1, "Ana", "secret plans");
        assert_eq!(
            arrivals.view(ArrivalNotice::NameOnly).unwrap().detail,
            "New message"
        );
        assert!(arrivals.view(ArrivalNotice::Off).is_none());
    }

    #[test]
    fn several_messages_collapse_into_one_panel() {
        let mut arrivals = Arrivals::default();
        arrivals.push(1, "Ana", "one");
        arrivals.push(1, "Ana", "two");
        arrivals.push(1, "Ana", "three");
        let view = arrivals.view(ArrivalNotice::Preview).unwrap();
        assert_eq!(
            (view.title.as_str(), view.detail.as_str()),
            ("Ana", "3 new messages")
        );
        arrivals.push(2, "Bob", "hey");
        let view = arrivals.view(ArrivalNotice::Preview).unwrap();
        assert_eq!(view.title, "Bob + 1 more");
        assert_eq!(view.contact_id, 2);
        assert_eq!(view.count, 4);
    }

    #[test]
    fn reading_a_conversation_clears_it_from_the_panel() {
        let mut arrivals = Arrivals::default();
        arrivals.push(1, "Ana", "one");
        arrivals.push(2, "Bob", "two");
        arrivals.clear_contact(2);
        assert_eq!(arrivals.view(ArrivalNotice::Preview).unwrap().title, "Ana");
        arrivals.clear_contact(1);
        assert!(arrivals.is_empty());
        assert!(arrivals.view(ArrivalNotice::Preview).is_none());
    }

    #[test]
    fn preview_is_one_line_and_cut_on_characters() {
        assert_eq!(preview("first line\nsecond line"), "first line…");
        assert_eq!(preview("\n\n  hello  "), "hello");
        let long = "é".repeat(PREVIEW_CHARS + 10);
        let cut = preview(&long);
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(
            preview(&"a".repeat(PREVIEW_CHARS)),
            "a".repeat(PREVIEW_CHARS)
        );
    }
}
