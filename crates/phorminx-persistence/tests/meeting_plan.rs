use phorminx_persistence::{MeetingPlanDraft, Persistence, RetentionPolicy};

fn fixture() -> (tempfile::TempDir, Persistence) {
    let dir = tempfile::tempdir().unwrap();
    let db = Persistence::open(dir.path().join("plans.db")).unwrap();
    db.history()
        .set_retention(RetentionPolicy::Indefinite, 0)
        .unwrap();
    (dir, db)
}

fn draft(title: &str, at: Option<i64>) -> MeetingPlanDraft {
    MeetingPlanDraft {
        title: title.into(),
        scheduled_at_ms: at,
        reminder_minutes: Some(10),
        instructions: "Keep answers short.".into(),
        briefing: String::new(),
        items: vec![
            ("Budget cap is 40k".into(), false),
            ("Ask about Friday".into(), true),
        ],
    }
}

#[test]
fn plans_round_trip_with_ordered_items_and_upcoming_order() {
    let (_dir, db) = fixture();
    let plans = db.meeting_plans();
    let late = plans.create(&draft("Late", Some(2_000_000)), 1).unwrap();
    let draft_only = plans.create(&draft("Someday", None), 1).unwrap();
    let soon = plans.create(&draft("Soon", Some(1_000_000)), 1).unwrap();
    let ids: Vec<i64> = plans.upcoming(10).unwrap().iter().map(|p| p.id).collect();
    assert_eq!(ids, [soon, late, draft_only]);
    let plan = plans.get(soon).unwrap().unwrap();
    assert_eq!(plan.items.len(), 2);
    assert_eq!(plan.items[0].text, "Budget cap is 40k");
    assert!(plan.items[1].done);

    plans
        .set_items(soon, &[("Only this".into(), false)], 5)
        .unwrap();
    assert_eq!(plans.get(soon).unwrap().unwrap().items.len(), 1);

    let session = db.meetings().create("Soon", "system", 9).unwrap().unwrap();
    plans.link_session(soon, session).unwrap();
    assert_eq!(plans.for_session(session).unwrap().unwrap().id, soon);
    assert!(plans.upcoming(10).unwrap().iter().all(|p| p.id != soon));
}

#[test]
fn reminders_fire_once_and_rearm_when_the_time_changes() {
    let (_dir, db) = fixture();
    let plans = db.meeting_plans();
    let id = plans.create(&draft("Sync", Some(3_600_000)), 1).unwrap();
    assert!(plans.due_reminders(0).unwrap().is_empty());
    // Ten minutes before.
    assert_eq!(plans.due_reminders(3_000_000).unwrap().len(), 1);
    plans.mark_reminded(id).unwrap();
    assert!(plans.due_reminders(3_000_000).unwrap().is_empty());
    let mut moved = draft("Sync", Some(7_200_000));
    moved.briefing = "brief".into();
    plans.update(id, &moved, 2).unwrap();
    assert_eq!(plans.due_reminders(6_600_000).unwrap().len(), 1);
}

#[test]
fn invalid_plans_are_rejected_and_deleting_cascades_items() {
    let (_dir, db) = fixture();
    let plans = db.meeting_plans();
    assert!(plans.create(&draft("  ", None), 1).is_err());
    let mut many = draft("Many", None);
    many.items = (0..21).map(|i| (format!("item {i}"), false)).collect();
    assert!(plans.create(&many, 1).is_err());
    let mut blank = draft("Blank item", None);
    blank.items = vec![(" ".into(), false)];
    assert!(plans.create(&blank, 1).is_err());
    let id = plans.create(&draft("Gone", None), 1).unwrap();
    assert!(plans.delete(id).unwrap());
    assert!(plans.get(id).unwrap().is_none());
}
