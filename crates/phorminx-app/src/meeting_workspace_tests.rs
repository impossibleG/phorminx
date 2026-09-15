//! Adversarial coordinator checks: synthetic events, temporary settings/database,
//! no microphone, model inference, external requests or personal data.
use super::*;
use crate::settings::Settings;
use phorminx_persistence::RetentionPolicy;

struct Fixture {
    host: Host,
    directory: tempfile::TempDir,
    id: i64,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
    let mut settings = Settings::default();
    settings.privacy.history_retention = HistoryRetention::Indefinite;
    store.save(&settings).unwrap();
    let mut host = Host::new(store).unwrap();
    host.db
        .history()
        .set_retention(RetentionPolicy::Indefinite, now_ms())
        .unwrap();
    let id = host
        .db
        .meetings()
        .create("Synthetic meeting", "system", now_ms())
        .unwrap()
        .unwrap();
    host.view.session_id = Some(id);
    host.config.provider.model = "synthetic-chat".into();
    host.config.preset = "Help with this meeting.".into();
    Fixture {
        host,
        directory,
        id,
    }
}

#[test]
fn recoverable_audio_warning_does_not_stop_or_reset_the_meeting() {
    let mut f = fixture();
    f.host.view.capture = CaptureState::Listening;
    segment(&mut f.host, 0, 10, "Before warning");
    f.host
        .audio_event(MeetingAudioEvent::Warning {
            message: "Synthetic timing discontinuity; capture continues.".into(),
        })
        .unwrap();
    assert_eq!(f.host.view.capture, CaptureState::Listening);
    assert_eq!(f.host.view.session_id, Some(f.id));
    segment(&mut f.host, 10, 20, "After warning");
    flush(&mut f.host);
    assert_eq!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .len(),
        2
    );
    assert!(
        f.host
            .db
            .meetings()
            .get(f.id)
            .unwrap()
            .unwrap()
            .ended_at_ms
            .is_none()
    );
}

#[test]
fn deleting_transcripts_drains_old_writes_and_preserves_chat() {
    let mut f = fixture();
    segment(&mut f.host, 0, 10, "Transcript to remove");
    f.host
        .db
        .meetings()
        .append_chat_user(f.id, "Chat to keep", now_ms())
        .unwrap();
    f.host
        .delete_selected(phorminx_persistence::DeleteSelection {
            meeting_transcripts: true,
            ..Default::default()
        })
        .unwrap();
    assert!(f.host.pending_segments.is_empty());
    assert!(f.host.view.transcript.is_empty());
    assert_eq!(f.host.boundaries.committed_sample(), 0);
    assert!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.host.db.meetings().messages(f.id, None, 100).unwrap()[0].text,
        "Chat to keep"
    );
    flush(&mut f.host);
    assert!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn active_answer_blocks_selected_meeting_deletion_without_cancelling_it() {
    let mut f = fixture();
    segment(&mut f.host, 0, 10, "Keep this transcript");
    f.host.submit("Active question".into(), None).unwrap();
    assert!(
        f.host
            .delete_selected(phorminx_persistence::DeleteSelection {
                chats: true,
                ..Default::default()
            })
            .is_err()
    );
    assert!(f.host.pending.is_some());
    assert!(f.host.view.ai_busy);
    assert_eq!(f.host.view.transcript[0].text, "Keep this transcript");
    f.host.cancel_answer();
    f.host
        .delete_selected(phorminx_persistence::DeleteSelection {
            chats: true,
            ..Default::default()
        })
        .unwrap();
    assert!(
        f.host
            .db
            .meetings()
            .messages(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .len(),
        1
    );
}

fn segment(host: &mut Host, start_sample: u64, end_sample: u64, text: &str) {
    host.audio_event(MeetingAudioEvent::Segment {
        start_sample,
        end_sample,
        text: text.into(),
    })
    .unwrap();
}

fn flush(host: &mut Host) {
    for _ in 0..100 {
        host.flush_persistence();
        if host.pending_segments.is_empty() {
            return;
        }
    }
    panic!("synthetic persistence queue did not drain");
}

#[test]
fn stop_persists_queued_tail_before_marking_session_ended() {
    let mut f = fixture();
    segment(&mut f.host, 0, 16000, "last words");
    f.host
        .audio_event(MeetingAudioEvent::Stopped { sample: 16000 })
        .unwrap();
    assert!(
        f.host
            .db
            .meetings()
            .get(f.id)
            .unwrap()
            .unwrap()
            .ended_at_ms
            .is_none()
    );
    assert!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
    flush(&mut f.host);
    let saved = f.host.db.meetings().get(f.id).unwrap().unwrap();
    assert_eq!(saved.committed_sample, 16000);
    assert!(saved.ended_at_ms.is_some());
    assert_eq!(
        f.host.db.meetings().segments(f.id, None, 100).unwrap()[0].text,
        "last words"
    );
}

#[test]
fn database_contention_does_not_drop_tail_or_finalize_before_retry() {
    let mut f = fixture();
    segment(&mut f.host, 0, 10, "first");
    segment(&mut f.host, 10, 20, "tail");
    f.host
        .audio_event(MeetingAudioEvent::Stopped { sample: 20 })
        .unwrap();
    let blocker = rusqlite::Connection::open(f.directory.path().join("phorminx.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    f.host.flush_persistence();
    assert_eq!(f.host.pending_segments.len(), 3);
    assert!(
        f.host
            .db
            .meetings()
            .get(f.id)
            .unwrap()
            .unwrap()
            .ended_at_ms
            .is_none()
    );
    blocker.execute_batch("ROLLBACK").unwrap();
    flush(&mut f.host);
    assert_eq!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .len(),
        2
    );
    assert!(
        f.host
            .db
            .meetings()
            .get(f.id)
            .unwrap()
            .unwrap()
            .ended_at_ms
            .is_some()
    );
}

#[test]
fn cutoff_user_message_waits_behind_its_transcript_segments() {
    let mut f = fixture();
    f.host.boundaries.request_cutoff(1, 20).unwrap();
    segment(&mut f.host, 0, 10, "before");
    segment(&mut f.host, 10, 20, "cutoff");
    f.host
        .audio_event(MeetingAudioEvent::CutoffReached { id: 1, sample: 20 })
        .unwrap();
    assert!(f.host.pending.is_some()); // Deliberately do not start a network worker.
    assert!(
        f.host
            .db
            .meetings()
            .messages(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
    flush(&mut f.host);
    let messages = f.host.db.meetings().messages(f.id, None, 100).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].cutoff_sample, Some(20));
    assert_eq!(messages[0].text, "before cutoff");
    assert_eq!(messages[1].status, MeetingMessageStatus::Streaming);
}

#[test]
fn missing_provider_keeps_cutoff_retryable_while_new_speech_accumulates() {
    let mut f = fixture();
    f.host.boundaries.request_cutoff(1, 10).unwrap();
    f.host.cutoff_id = 1;
    segment(&mut f.host, 0, 10, "retry this");
    f.host.config.provider.model.clear();
    assert!(
        f.host
            .audio_event(MeetingAudioEvent::CutoffReached { id: 1, sample: 10 })
            .is_err()
    );
    assert_eq!(f.host.deferred_cutoff, Some((1, 10)));
    assert_eq!(f.host.boundaries.submitted_sample(), 0);
    segment(&mut f.host, 10, 20, "next portion");
    f.host.config.provider.model = "synthetic-chat".into();
    f.host.send_cutoff().unwrap();
    assert_eq!(f.host.boundaries.submitted_sample(), 10);
    assert_eq!(f.host.deferred_cutoff, None);
    f.host.send_cutoff().unwrap();
    assert_eq!(f.host.boundaries.submitted_sample(), 20);
    flush(&mut f.host);
    let user_texts: Vec<_> = f
        .host
        .db
        .meetings()
        .messages(f.id, None, 100)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == "user")
        .map(|m| m.text)
        .collect();
    assert_eq!(user_texts, ["retry this", "next portion"]);
}

#[test]
fn context_budget_includes_preset_and_does_not_consume_rejected_portion() {
    let mut f = fixture();
    let text = "a".repeat(CONTEXT_BYTES);
    f.host.boundaries.request_cutoff(1, 10).unwrap();
    segment(&mut f.host, 0, 10, &text);
    assert!(
        f.host
            .audio_event(MeetingAudioEvent::CutoffReached { id: 1, sample: 10 })
            .is_err()
    );
    assert_eq!(f.host.boundaries.submitted_sample(), 0);
    assert!(f.host.view.messages.is_empty());
    f.host.config.preset.clear();
    f.host.send_cutoff().unwrap();
    assert_eq!(f.host.boundaries.submitted_sample(), 10);
    let pending = f.host.pending.as_ref().unwrap();
    assert_eq!(pending.messages.last().unwrap().role, ChatRole::User);
    assert_eq!(pending.messages.last().unwrap().content, text);
}

#[test]
fn cancelling_an_unstarted_answer_persists_cancelled_not_streaming() {
    let mut f = fixture();
    f.host.submit("A question".into(), None).unwrap();
    let generation = f.host.pending.as_ref().unwrap().generation;
    f.host.cancel_answer();
    assert!(f.host.pending.is_none());
    assert!(!f.host.runs.accepts(generation));
    flush(&mut f.host);
    let messages = f.host.db.meetings().messages(f.id, None, 100).unwrap();
    assert_eq!(
        messages.last().unwrap().status,
        MeetingMessageStatus::Cancelled
    );
    assert_eq!(f.host.view.messages.last().unwrap().status, "stopped");
}

#[test]
fn stale_tokens_and_completion_cannot_modify_replacement_answer() {
    let mut f = fixture();
    f.host.submit("First question".into(), None).unwrap();
    let first = f.host.pending.take().unwrap();
    f.host.active = Some(ActiveJob {
        generation: first.generation,
        cancel: CancellationToken::default(),
        session: first.session,
    });
    f.host
        .job_event(JobEvent::Delta(first.generation, "partial first".into()));
    f.host.submit("Second question".into(), None).unwrap();
    let second = f.host.pending.as_ref().unwrap().generation;
    f.host
        .job_event(JobEvent::Delta(first.generation, "STALE".into()));
    f.host.job_event(JobEvent::End(first.generation, Ok(())));
    assert!(f.host.runs.accepts(second));
    assert!(f.host.view.messages.last().unwrap().text.is_empty());
    assert_eq!(f.host.view.messages.last().unwrap().status, "waiting");
    flush(&mut f.host);
    let messages = f.host.db.meetings().messages(f.id, None, 100).unwrap();
    assert_eq!(messages[1].text, "partial first");
    assert_eq!(messages[1].status, MeetingMessageStatus::Cancelled);
    assert_eq!(messages[3].status, MeetingMessageStatus::Streaming);
}

#[test]
fn saved_generation_is_advanced_before_followup_and_privacy_blocks_reopen() {
    let mut f = fixture();
    f.host
        .db
        .meetings()
        .append_chat_user(f.id, "saved question", now_ms())
        .unwrap();
    f.host
        .db
        .meetings()
        .start_assistant(f.id, 40, now_ms())
        .unwrap();
    f.host
        .db
        .meetings()
        .update_assistant(
            f.id,
            40,
            "saved answer",
            MeetingMessageStatus::Complete,
            now_ms(),
        )
        .unwrap();
    f.host.open_session(f.id).unwrap();
    f.host.submit("followup".into(), None).unwrap();
    assert_eq!(f.host.pending.as_ref().unwrap().generation, 41);
    flush(&mut f.host);
    assert_eq!(
        f.host
            .db
            .meetings()
            .get(f.id)
            .unwrap()
            .unwrap()
            .assistant_generation,
        41
    );
    f.host.new_session().unwrap();
    let mut settings = f.host.store.load().unwrap();
    settings.privacy.history_retention = HistoryRetention::Disabled;
    f.host.store.save(&settings).unwrap();
    assert!(f.host.open_session(f.id).is_err());
    assert!(f.host.view.transcript.is_empty());
    assert!(f.host.view.messages.is_empty());
}

#[test]
fn deleting_session_before_queue_flush_never_recreates_private_text() {
    let mut f = fixture();
    segment(&mut f.host, 0, 10, "private audio text");
    f.host.submit("private typed text".into(), None).unwrap();
    f.host.db.meetings().delete(f.id).unwrap();
    flush(&mut f.host);
    assert!(f.host.db.meetings().get(f.id).unwrap().is_none());
    assert!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
    assert!(
        f.host
            .db
            .meetings()
            .messages(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unchanged_snapshots_do_not_advance_revision_or_publish_again() {
    let mut f = fixture();
    let shared = Arc::new(Mutex::new(WorkspaceSnapshot::default()));
    f.host.publish_snapshot(&shared);
    let first = shared.lock().unwrap().revision;
    assert!(first > 0);
    for _ in 0..20 {
        f.host.publish_snapshot(&shared);
    }
    assert_eq!(shared.lock().unwrap().revision, first);
    f.host.view.notice = "Changed".into();
    f.host.publish_snapshot(&shared);
    assert_eq!(shared.lock().unwrap().revision, first + 1);
    assert_eq!(shared.lock().unwrap().notice, "Changed");
}

#[test]
fn shutdown_storage_stall_remains_retryable_and_never_reports_ready_early() {
    let mut f = fixture();
    segment(&mut f.host, 0, 10, "Pending final words.");
    f.host
        .audio_event(MeetingAudioEvent::Stopped { sample: 10 })
        .unwrap();
    let blocker = rusqlite::Connection::open(f.directory.path().join("phorminx.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut count = f.host.pending_segments.len();
    let mut stalled = Instant::now() - Duration::from_secs(6);
    assert!(!f.host.drain_shutdown(&mut count, &mut stalled));
    assert!(!f.host.view.shutdown_ready);
    assert!(!f.host.view.shutdown_error.is_empty());
    assert_eq!(f.host.pending_segments.len(), 2);
    blocker.execute_batch("ROLLBACK").unwrap();
    for _ in 0..10 {
        if f.host.drain_shutdown(&mut count, &mut stalled) {
            break;
        }
    }
    assert!(f.host.view.shutdown_ready);
    assert!(f.host.view.shutdown_error.is_empty());
    assert!(
        f.host
            .db
            .meetings()
            .get(f.id)
            .unwrap()
            .unwrap()
            .ended_at_ms
            .is_some()
    );
    assert_eq!(
        f.host.db.meetings().segments(f.id, None, 100).unwrap()[0].text,
        "Pending final words."
    );
}

#[test]
fn provisional_words_are_visible_but_never_persisted_or_submitted() {
    let mut f = fixture();
    f.host
        .audio_event(MeetingAudioEvent::Partial {
            start_sample: 0,
            text: "Unsealed prediction".into(),
        })
        .unwrap();
    assert_eq!(f.host.view.provisional_text, "Unsealed prediction");
    assert!(f.host.pending_segments.is_empty());
    assert_eq!(f.host.boundaries.committed_sample(), 0);
    segment(&mut f.host, 0, 10, "Final words");
    assert!(f.host.view.provisional_text.is_empty());
    f.host
        .audio_event(MeetingAudioEvent::Partial {
            start_sample: 0,
            text: "stale".into(),
        })
        .unwrap();
    assert!(f.host.view.provisional_text.is_empty());
    flush(&mut f.host);
    assert_eq!(
        f.host.db.meetings().segments(f.id, None, 100).unwrap()[0].text,
        "Final words"
    );
}

#[test]
fn disabling_retention_before_database_policy_sync_blocks_queued_writes() {
    let mut f = fixture();
    segment(&mut f.host, 0, 10, "must stay ephemeral");
    f.host.submit("private message".into(), None).unwrap();
    let mut settings = f.host.store.load().unwrap();
    settings.privacy.history_retention = HistoryRetention::Disabled;
    f.host.store.save(&settings).unwrap();
    // The persistence policy has not caught up yet. The host must still honor
    // the committed UI privacy setting before attempting any write.
    assert_eq!(
        f.host.db.history().retention().unwrap(),
        RetentionPolicy::Indefinite
    );
    flush(&mut f.host);
    assert_eq!(f.host.view.session_id, None);
    assert!(
        f.host
            .db
            .meetings()
            .segments(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
    assert!(
        f.host
            .db
            .meetings()
            .messages(f.id, None, 100)
            .unwrap()
            .is_empty()
    );
}
