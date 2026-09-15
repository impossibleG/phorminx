//! Synthetic host-level lifecycle regression tests. No recording or network workers start.
use super::*;
fn fixture() -> (tempfile::TempDir, Host, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(dir.path().join("settings.toml")).unwrap();
    let mut settings = crate::settings::Settings::default();
    settings.privacy.history_retention = HistoryRetention::Indefinite;
    store.save(&settings).unwrap();
    let mut host = Host::new(store).unwrap();
    host.db
        .history()
        .set_retention(phorminx_persistence::RetentionPolicy::Indefinite, 0)
        .unwrap();
    let id = host
        .db
        .meetings()
        .create("Synthetic meeting", "system", 0)
        .unwrap()
        .unwrap();
    host.view.session_id = Some(id);
    host.view.history_enabled = true;
    host.config.provider.model = "synthetic-local-model".into();
    (dir, host, id)
}
fn segment(host: &mut Host, start: u64, end: u64, text: &str) {
    host.audio_event(MeetingAudioEvent::Segment {
        start_sample: start,
        end_sample: end,
        text: text.into(),
    })
    .unwrap();
}
fn flush(host: &mut Host) {
    for _ in 0..20 {
        host.flush_persistence();
        if host.pending_segments.is_empty() {
            return;
        }
    }
    panic!("synthetic persistence did not drain");
}
#[test]
fn questions_attach_only_new_context_and_persist_separate_visible_text() {
    let (_dir, mut host, id) = fixture();
    segment(&mut host, 0, 10, "First portion");
    host.send_question("What is next?".into()).unwrap();
    let question = host.view.messages.iter().find(|m| m.role == "You").unwrap();
    assert_eq!(question.text, "What is next?");
    assert_eq!(question.context_text, "First portion");
    assert_eq!(question.context_samples, Some((0, 10)));
    assert_eq!(host.boundaries.submitted_sample(), 10);
    segment(&mut host, 10, 20, "Second portion");
    host.send_question("And now?".into()).unwrap();
    let question = host
        .view
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "You")
        .unwrap();
    assert_eq!(question.context_text, "Second portion");
    assert_eq!(question.context_samples, Some((10, 20)));
    flush(&mut host);
    let messages = host.db.meetings().messages(id, None, 20).unwrap();
    let questions: Vec<_> = messages.iter().filter(|m| m.role == "user").collect();
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].text, "What is next?");
    assert_eq!(
        host.db
            .meetings()
            .question_context(id, questions[0].id)
            .unwrap()
            .unwrap()
            .text,
        "First portion"
    );
}
#[test]
fn question_without_new_audio_is_plain_chat_not_duplicate_attachment() {
    let (_dir, mut host, id) = fixture();
    segment(&mut host, 0, 10, "Original portion");
    host.send_question("First?".into()).unwrap();
    host.send_question("Follow up?".into()).unwrap();
    let question = host
        .view
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "You")
        .unwrap();
    assert!(question.context_text.is_empty());
    assert!(question.context_samples.is_none());
    assert_eq!(host.boundaries.submitted_sample(), 10);
    flush(&mut host);
    let messages = host.db.meetings().messages(id, None, 20).unwrap();
    let latest = messages.iter().rev().find(|m| m.role == "user").unwrap();
    assert!(
        host.db
            .meetings()
            .question_context(id, latest.id)
            .unwrap()
            .is_none()
    );
}
#[test]
fn rejected_question_preserves_transcript_boundary_and_no_pending_model_call() {
    let (_dir, mut host, _id) = fixture();
    segment(&mut host, 0, 10, "Unsent context");
    host.config.provider.model.clear();
    assert!(host.send_question("What next?".into()).is_err());
    assert_eq!(host.boundaries.submitted_sample(), 0);
    assert!(host.pending.is_none());
    assert!(host.view.messages.is_empty());
}
#[test]
fn failed_action_capture_never_sends_and_late_stop_cannot_send() {
    let (_dir, mut host, _id) = fixture();
    host.note_capture = true;
    host.view.note_text = "Synthetic draft".into();
    host.action_capture = Some(ActionConfig {
        id: "test".into(),
        name: "Test".into(),
        ..Default::default()
    });
    host.audio_event(MeetingAudioEvent::Error {
        message: "Synthetic capture failure".into(),
    })
    .unwrap();
    assert!(host.action_capture.is_none());
    assert!(!host.view.delivery_busy);
    host.audio_event(MeetingAudioEvent::Stopped { sample: 0 })
        .unwrap();
    assert!(!host.view.delivery_busy);
    assert!(host.last_delivery.is_none());
}

#[test]
fn live_pending_cutoff_seals_exact_context_and_later_audio_stays_unsent() {
    let (_dir, mut host, id) = fixture();
    segment(&mut host, 0, 10, "Before request");
    host.cutoff_id = 1;
    host.boundaries.request_cutoff(1, 20).unwrap();
    host.cutoff_question = Some("Explain this".into());
    host.view.send_pending = true;
    segment(&mut host, 10, 20, "At boundary");
    // Audio can continue committing while a boundary notification waits in the host queue.
    segment(&mut host, 20, 30, "Only for the next question");
    host.audio_event(MeetingAudioEvent::CutoffReached { id: 1, sample: 20 })
        .unwrap();
    let question = host.view.messages.iter().find(|m| m.role == "You").unwrap();
    assert_eq!(question.context_text, "Before request At boundary");
    assert_eq!(question.context_samples, Some((0, 20)));
    assert!(!host.view.send_pending);
    assert_eq!(host.boundaries.submitted_sample(), 20);
    host.send_question("What followed?".into()).unwrap();
    let question = host
        .view
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "You")
        .unwrap();
    assert_eq!(question.context_text, "Only for the next question");
    assert_eq!(question.context_samples, Some((20, 30)));
    flush(&mut host);
    assert_eq!(
        host.db
            .meetings()
            .messages(id, None, 20)
            .unwrap()
            .iter()
            .filter(|m| m.role == "user")
            .count(),
        2
    );
}

#[test]
fn silent_cutoff_questions_persist_as_plain_messages_and_advance_audio() {
    let (_dir, mut host, id) = fixture();
    for marker in [10, 20] {
        segment(&mut host, marker - 10, marker, "  ");
        host.send_question(format!("Question at {marker}")).unwrap();
        assert_eq!(host.boundaries.submitted_sample(), marker);
    }
    flush(&mut host);
    let rows = host.db.meetings().messages(id, None, 20).unwrap();
    let users: Vec<_> = rows.iter().filter(|m| m.role == "user").collect();
    assert_eq!(users.len(), 2);
    for row in users {
        assert!(row.cutoff_sample.is_none());
        assert!(
            host.db
                .meetings()
                .question_context(id, row.id)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn oversized_combined_context_stays_deferred_and_can_retry_shorter_question() {
    let (_dir, mut host, _id) = fixture();
    host.config.preset.clear();
    segment(&mut host, 0, 10, &"x".repeat(CONTEXT_BYTES - 256));
    assert!(host.send_question("q".repeat(512)).is_err());
    assert_eq!(host.boundaries.submitted_sample(), 0);
    assert_eq!(host.deferred_cutoff, Some((1, 10)));
    assert!(host.pending.is_none());
    assert!(host.view.messages.is_empty());
    host.cutoff_question = Some("Summarize".into());
    host.accept_deferred_cutoff().unwrap();
    assert_eq!(host.boundaries.submitted_sample(), 10);
    assert!(host.deferred_cutoff.is_none());
    assert!(host.pending.is_some());
}

#[test]
fn invalid_new_question_does_not_cancel_existing_pending_answer() {
    let (_dir, mut host, _id) = fixture();
    host.send_question("Existing question".into()).unwrap();
    let generation = host.pending.as_ref().unwrap().generation;
    host.config.provider.model.clear();
    assert!(host.send_question("Invalid next question".into()).is_err());
    assert_eq!(host.pending.as_ref().unwrap().generation, generation);
    assert!(host.view.ai_busy);
    assert_eq!(host.view.messages.last().unwrap().status, "waiting");
}

#[test]
fn rejected_second_draft_cannot_receive_first_pending_cutoff_acknowledgement() {
    let (_dir, mut host, _id) = fixture();
    segment(&mut host, 0, 10, "First context");
    host.cutoff_id = 1;
    host.boundaries.request_cutoff(1, 20).unwrap();
    host.cutoff_question = Some("Question A".into());
    host.cutoff_request = Some(41);
    host.view.send_pending = true;
    assert!(
        host.handle(WorkspaceEvent::SendChatRequest {
            id: 42,
            text: "Question B retained in composer".into()
        })
        .is_err()
    );
    assert_eq!(host.view.chat_error_id, 42);
    assert_eq!(host.cutoff_request, Some(41));
    assert_eq!(host.view.chat_ack_id, 0);
    segment(&mut host, 10, 20, "Remaining A context");
    host.audio_event(MeetingAudioEvent::CutoffReached { id: 1, sample: 20 })
        .unwrap();
    assert_eq!(host.view.chat_ack_id, 41);
    assert_eq!(host.view.chat_error_id, 42);
    assert!(host.cutoff_request.is_none());
    assert_eq!(
        host.view
            .messages
            .iter()
            .filter(|line| line.role == "You")
            .count(),
        1
    );
    assert_eq!(
        host.view
            .messages
            .iter()
            .find(|line| line.role == "You")
            .unwrap()
            .text,
        "Question A"
    );
}

#[test]
fn disabling_history_blocks_cached_saved_context_before_periodic_refresh() {
    let (_dir, mut host, _id) = fixture();
    host.loaded_session = true;
    host.view.saved_session = true;
    host.view.messages.push(ChatLine {
        role: "You".into(),
        text: "Saved question".into(),
        context_text: "Private cached saved transcript".into(),
        context_samples: Some((0, 10)),
        status: "complete".into(),
    });
    let mut settings = host.store.load().unwrap();
    settings.privacy.history_retention = HistoryRetention::Disabled;
    host.store.save(&settings).unwrap();
    assert!(host.send_question("Use my prior context".into()).is_err());
    assert!(host.pending.is_none());
    assert!(!host.view.ai_busy);
}
