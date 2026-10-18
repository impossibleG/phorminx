use phorminx_persistence::{
    DeleteSelection, EdgeStatus, ImageReference, InvestigationItemKind as Kind,
    InvestigationMessageStatus as MessageStatus, LinkPreviewUpdate, NewInvestigationItem,
    OcrLineRecord, Persistence, PersistenceError, RetentionPolicy, WorkState,
};

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn fixture() -> (tempfile::TempDir, Persistence, i64) {
    let dir = tempfile::tempdir().unwrap();
    let db = Persistence::open(dir.path().join("test.sqlite3")).unwrap();
    let id = db
        .investigations()
        .create("  Supplier \n audit ", 100)
        .unwrap();
    (dir, db, id)
}

fn image(sha: &str) -> NewInvestigationItem {
    NewInvestigationItem {
        image: Some(ImageReference {
            sha256: sha.into(),
            width: 640,
            height: 480,
        }),
        source: "paste".into(),
        ..NewInvestigationItem::new(Kind::Image, "")
    }
}

#[test]
fn investigations_are_created_listed_renamed_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sqlite3");
    {
        let db = Persistence::open(&path).unwrap();
        let repo = db.investigations();
        let first = repo.create("  Supplier \n audit ", 100).unwrap();
        let second = repo.create("", 200).unwrap();
        assert_eq!(repo.get(first).unwrap().unwrap().title, "Supplier audit");
        assert_eq!(
            repo.get(second).unwrap().unwrap().title,
            "Untitled investigation"
        );
        repo.add_item(first, &NewInvestigationItem::new(Kind::Note, "hello"), 300)
            .unwrap();
        assert!(repo.rename(second, "Renamed", 250).unwrap());
        let list = repo.list().unwrap();
        assert_eq!(
            list.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!(list[0].item_count, 1);
    }
    let db = Persistence::open(&path).unwrap();
    assert_eq!(db.investigations().list().unwrap().len(), 2);
}

#[test]
fn items_keep_chronology_and_exact_search_covers_text_links_and_ocr() {
    let (_dir, db, id) = fixture();
    let repo = db.investigations();
    let note = repo
        .add_item(
            id,
            &NewInvestigationItem::new(Kind::Note, "Call the Café owner"),
            10,
        )
        .unwrap();
    let link = repo
        .add_item(
            id,
            &NewInvestigationItem {
                url: Some("https://example.com/report".into()),
                ..NewInvestigationItem::new(Kind::Link, "")
            },
            20,
        )
        .unwrap();
    let screenshot = repo.add_item(id, &image(SHA_A), 30).unwrap();
    assert_eq!(
        repo.items(id, None, None)
            .unwrap()
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        vec![note, link, screenshot]
    );
    assert_eq!(repo.items(id, None, Some("café")).unwrap()[0].id, note);
    assert!(repo.items(id, None, Some("cafe")).unwrap().is_empty());
    assert_eq!(repo.items(id, Some(Kind::Link), None).unwrap()[0].id, link);
    assert_eq!(
        repo.items(id, None, Some("example.com")).unwrap()[0].id,
        link
    );
    repo.save_link_preview(
        link,
        &LinkPreviewUpdate {
            title: Some("Annual   Report".into()),
            description: Some("Revenue grew".into()),
            site: Some("Example".into()),
            image: None,
            favicon_sha256: Some(SHA_B.into()),
        },
        40,
    )
    .unwrap();
    let saved = repo.item(link).unwrap().unwrap();
    assert_eq!(saved.link_title.as_deref(), Some("Annual Report"));
    assert_eq!(saved.fetch_state, Some(WorkState::Done));
    assert_eq!(
        repo.items(id, None, Some("revenue GREW")).unwrap()[0].id,
        link
    );
    let lines = [OcrLineRecord {
        text: "Invoice 4821".into(),
        confidence: 0.98,
        left: 1,
        top: 2,
        width: 3,
        height: 4,
    }];
    assert!(repo.save_ocr(screenshot, &lines, 50).unwrap());
    assert_eq!(
        repo.items(id, None, Some("invoice 4821")).unwrap()[0].id,
        screenshot
    );
    assert_eq!(repo.ocr_lines(screenshot).unwrap(), lines);
}

#[test]
fn ocr_queue_is_ordered_and_never_overwrites_user_edits() {
    let (_dir, db, id) = fixture();
    let repo = db.investigations();
    let first = repo.add_item(id, &image(SHA_A), 10).unwrap();
    let second = repo.add_item(id, &image(SHA_B), 20).unwrap();
    assert_eq!(repo.pending_ocr_count().unwrap(), 2);
    let job = repo.next_pending_ocr().unwrap().unwrap();
    assert_eq!(
        (job.item_id, job.investigation_id, job.target.as_str()),
        (first, id, SHA_A)
    );
    repo.update_item_text(first, "", "my own words", 15)
        .unwrap();
    let line = OcrLineRecord {
        text: "Detected".into(),
        confidence: 0.9,
        left: 0,
        top: 0,
        width: 10,
        height: 10,
    };
    assert!(
        repo.save_ocr(first, std::slice::from_ref(&line), 30)
            .unwrap()
    );
    assert_eq!(repo.item(first).unwrap().unwrap().body, "my own words");
    // A late duplicate result for a finished item is ignored.
    assert!(!repo.save_ocr(first, &[line], 31).unwrap());
    assert!(repo.fail_ocr(second, 40).unwrap());
    assert_eq!(
        repo.item(second).unwrap().unwrap().ocr_state,
        Some(WorkState::Failed)
    );
    assert!(repo.next_pending_ocr().unwrap().is_none());
    assert!(repo.retry_ocr(second, 50).unwrap());
    assert_eq!(repo.next_pending_ocr().unwrap().unwrap().item_id, second);
}

#[test]
fn deleting_items_reports_only_unreferenced_images() {
    let (_dir, db, id) = fixture();
    let repo = db.investigations();
    let a = repo.add_item(id, &image(SHA_A), 10).unwrap();
    let duplicate = repo.add_item(id, &image(SHA_A), 11).unwrap();
    let b = repo.add_item(id, &image(SHA_B), 12).unwrap();
    assert_eq!(repo.delete_item(a, 20).unwrap(), Some((id, vec![])));
    assert_eq!(
        repo.delete_item(duplicate, 21).unwrap(),
        Some((id, vec![SHA_A.into()]))
    );
    assert_eq!(repo.image_hashes(id).unwrap(), vec![SHA_B.to_owned()]);
    assert_eq!(
        repo.delete_item(b, 22).unwrap(),
        Some((id, vec![SHA_B.into()]))
    );
    assert_eq!(repo.delete_item(b, 23).unwrap(), None);
}

#[test]
fn deleting_an_investigation_cascades_and_clears_quick_capture_target() {
    let (_dir, db, id) = fixture();
    let repo = db.investigations();
    let other = repo.create("Other", 10).unwrap();
    let item = repo.add_item(id, &image(SHA_A), 10).unwrap();
    let node = repo.place_item(item, 1.0, 2.0, 11).unwrap();
    let concept = repo.add_concept(id, "Theme", 5.0, 5.0, 12).unwrap();
    repo.add_edge(node, concept, EdgeStatus::Accepted, "", "", 13)
        .unwrap();
    repo.append_message(id, "user", MessageStatus::Complete, "Why?", 14)
        .unwrap();
    repo.set_active(Some(id)).unwrap();
    assert_eq!(repo.active().unwrap(), Some(id));
    assert!(repo.set_active(Some(9999)).is_err());
    assert!(repo.delete(id).unwrap());
    assert_eq!(repo.active().unwrap(), None);
    assert!(repo.item(item).unwrap().is_none());
    assert!(repo.nodes(id).unwrap().is_empty());
    assert!(repo.edges(id).unwrap().is_empty());
    assert!(repo.messages(id, 10).unwrap().is_empty());
    assert!(repo.get(other).unwrap().is_some());
}

#[test]
fn investigations_ignore_history_retention_and_category_deletion() {
    let (_dir, db, id) = fixture();
    db.history()
        .set_retention(RetentionPolicy::Disabled, 100)
        .unwrap();
    let repo = db.investigations();
    let note = repo
        .add_item(id, &NewInvestigationItem::new(Kind::Voice, "spoken"), 10)
        .unwrap();
    db.delete_selected(DeleteSelection {
        dictations: true,
        meeting_transcripts: true,
        chats: true,
    })
    .unwrap();
    assert_eq!(repo.item(note).unwrap().unwrap().body, "spoken");
}

#[test]
fn diagram_nodes_edges_and_suggestions_follow_their_rules() {
    let (_dir, db, id) = fixture();
    let repo = db.investigations();
    let other = repo.create("Other", 1).unwrap();
    let a = repo
        .add_item(id, &NewInvestigationItem::new(Kind::Note, "a"), 10)
        .unwrap();
    let b = repo
        .add_item(id, &NewInvestigationItem::new(Kind::Note, "b"), 11)
        .unwrap();
    let node_a = repo.place_item(a, 10.0, 20.0, 12).unwrap();
    assert_eq!(repo.place_item(a, 30.0, 40.0, 13).unwrap(), node_a);
    let node_b = repo.place_item(b, 0.0, 0.0, 14).unwrap();
    let foreign = repo.add_concept(other, "Elsewhere", 0.0, 0.0, 15).unwrap();
    let nodes = repo.nodes(id).unwrap();
    assert_eq!((nodes[0].x, nodes[0].y), (30.0, 40.0));
    assert!(repo.place_item(a, f64::NAN, 0.0, 16).is_err());
    assert!(
        repo.add_edge(node_a, node_a, EdgeStatus::Accepted, "", "", 17)
            .is_err()
    );
    assert!(
        repo.add_edge(node_a, foreign, EdgeStatus::Accepted, "", "", 17)
            .is_err()
    );
    let suggestion = repo
        .add_edge(
            node_a,
            node_b,
            EdgeStatus::Suggested,
            "",
            "Both mention b",
            18,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.add_edge(node_b, node_a, EdgeStatus::Accepted, "", "", 19)
            .unwrap(),
        None
    );
    assert!(repo.accept_edge(suggestion).unwrap());
    assert_eq!(repo.edges(id).unwrap()[0].status, EdgeStatus::Accepted);
    assert_eq!(repo.clear_suggestions(id).unwrap(), 0);
    // Unplacing an item removes its node and edges but keeps the item.
    assert!(repo.delete_node(node_b).unwrap());
    assert!(repo.edges(id).unwrap().is_empty());
    assert!(repo.item(b).unwrap().is_some());
    let concept = repo.add_concept(id, "  ", 1.0, 1.0, 20).unwrap();
    assert!(repo.rename_node(concept, "Pricing").unwrap());
    assert!(
        !repo
            .rename_node(node_a, "Items keep their own title")
            .unwrap()
    );
    assert_eq!(repo.nodes(id).unwrap()[1].label, "Pricing");
    assert!(repo.save_layout(id, r#"{"x":1,"y":2,"zoom":1.5}"#).unwrap());
    assert!(repo.save_layout(id, "[1]").is_err());
    assert_eq!(repo.layout(id).unwrap(), r#"{"x":1,"y":2,"zoom":1.5}"#);
}

#[test]
fn messages_stream_then_freeze_and_interrupted_answers_fail() {
    let (_dir, db, id) = fixture();
    let repo = db.investigations();
    let answer = repo
        .append_message(id, "assistant", MessageStatus::Streaming, "", 10)
        .unwrap();
    assert!(
        repo.update_message(answer, "Partial", MessageStatus::Streaming, 11)
            .unwrap()
    );
    assert!(
        repo.update_message(answer, "Done [#1]", MessageStatus::Complete, 12)
            .unwrap()
    );
    assert!(
        !repo
            .update_message(answer, "late", MessageStatus::Complete, 13)
            .unwrap()
    );
    assert_eq!(repo.messages(id, 10).unwrap()[0].text, "Done [#1]");
    let stuck = repo
        .append_message(id, "assistant", MessageStatus::Streaming, "", 14)
        .unwrap();
    assert_eq!(repo.fail_interrupted_messages(15).unwrap(), 1);
    assert_eq!(
        repo.message(stuck).unwrap().unwrap().1.status,
        MessageStatus::Failed
    );
    assert!(
        repo.append_message(id, "system", MessageStatus::Complete, "x", 16)
            .is_err()
    );
}

#[test]
fn item_validation_rejects_malformed_and_oversized_input() {
    let (_dir, db, id) = fixture();
    let repo = db.investigations();
    assert!(
        repo.add_item(id, &NewInvestigationItem::new(Kind::Image, ""), 1)
            .is_err()
    );
    assert!(
        repo.add_item(id, &NewInvestigationItem::new(Kind::Link, ""), 1)
            .is_err()
    );
    assert!(repo.add_item(id, &image("ABC"), 1).is_err());
    assert!(matches!(
        repo.add_item(
            id,
            &NewInvestigationItem::new(Kind::Text, "x".repeat(300 * 1024)),
            1
        ),
        Err(PersistenceError::TextLimitExceeded { .. })
    ));
    assert!(
        repo.add_item(9999, &NewInvestigationItem::new(Kind::Text, "x"), 1)
            .is_err()
    );
}

#[test]
fn capture_shortcut_distinguishes_unset_from_turned_off() {
    let (_dir, db, _id) = fixture();
    let repo = db.investigations();
    assert_eq!(repo.capture_shortcut().unwrap(), None);
    repo.set_capture_shortcut("").unwrap();
    assert_eq!(repo.capture_shortcut().unwrap().as_deref(), Some(""));
    repo.set_capture_shortcut("Ctrl+Alt+Shift+I").unwrap();
    assert_eq!(
        repo.capture_shortcut().unwrap().as_deref(),
        Some("Ctrl+Alt+Shift+I")
    );
    assert!(repo.set_capture_shortcut(&"x".repeat(81)).is_err());
}
