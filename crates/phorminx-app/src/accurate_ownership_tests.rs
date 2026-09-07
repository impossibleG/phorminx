use super::*;
use phorminx_app::incremental::{ChunkPlan, TimeRange};
use phorminx_whisper::TimedSegment;

fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn forced_plan(end: u64) -> ChunkPlan {
    ChunkPlan {
        id: DictationId(950),
        sequence: 0,
        range: TimeRange {
            start: Duration::ZERO,
            end: seconds(end),
        },
        stable_end: seconds(end),
        start_overlap: None,
        boundary: BoundaryKind::Forced,
    }
}

#[test]
fn confirmed_full_hypothesis_owns_its_represented_right_edge() {
    let mut accumulator = PartialAccumulator::default();
    let first = forced_plan(3);
    let clip = AudioClip::new(vec![0.1; 16_000 * 3], 16_000).unwrap();
    append_timestamp_stable_segments(
        &mut accumulator,
        first,
        &clip,
        &[TimedSegment {
            text: "alpha beta gamma delta".to_owned(),
            start: Duration::ZERO,
            end: seconds(3),
        }],
        0.003,
    );
    assert!(accumulator.text.is_empty());
    assert_eq!(
        accumulator.provisional.as_ref().unwrap().commit_through,
        seconds(3)
    );

    // The later whole segment crosses its own guard, so only confirmation of
    // the preceding full hypothesis can advance ownership during this call.
    let second = forced_plan(6);
    let clip = AudioClip::new(vec![0.1; 16_000 * 6], 16_000).unwrap();
    append_timestamp_stable_segments(
        &mut accumulator,
        second,
        &clip,
        &[TimedSegment {
            text: "alpha beta gamma delta epsilon zeta".to_owned(),
            start: Duration::ZERO,
            end: seconds(6),
        }],
        0.003,
    );
    assert_eq!(accumulator.text, "alpha beta gamma delta");
    assert_eq!(accumulator.accepted_through, seconds(3));
    assert_eq!(accumulator.unresolved_from, Some(seconds(3)));
}

#[test]
fn full_hypothesis_needs_right_context_before_ownership_transfer() {
    let hypothesis = AccurateHypothesis {
        text: "alpha beta gamma delta".to_owned(),
        commit_through: seconds(3),
        range: forced_plan(3).range,
    };
    let current = "alpha beta gamma delta epsilon";
    assert!(!hypothesis_is_confirmed(
        &hypothesis,
        current,
        TimeRange {
            start: Duration::ZERO,
            end: Duration::from_millis(4_999),
        }
    ));
    assert!(hypothesis_is_confirmed(
        &hypothesis,
        current,
        TimeRange {
            start: Duration::ZERO,
            end: seconds(5),
        }
    ));
    assert!(!hypothesis_is_confirmed(
        &hypothesis,
        current,
        TimeRange {
            start: seconds(3),
            end: seconds(6),
        }
    ));
}

#[test]
fn bounded_repair_starts_at_current_ownership_not_obsolete_plan_start() {
    let range = accurate_repair_window(0, 16_000 * 40, 16_000 * 20).unwrap();
    assert_eq!(range, SampleRange::new(16_000 * 18, 16_000 * 30).unwrap());
    assert_eq!(range.len(), MAX_ACCURATE_LIVE_REPAIR_SAMPLES);
    assert!(range.end() > 16_000 * 20);

    let near_end = accurate_repair_window(16_000 * 10, 16_000 * 20, 16_000 * 29).unwrap();
    assert_eq!(
        near_end,
        SampleRange::new(16_000 * 27, 16_000 * 30).unwrap()
    );
    let at_start = accurate_repair_window(16_000 * 10, 16_000 * 20, 16_000 * 10).unwrap();
    assert_eq!(
        at_start,
        SampleRange::new(16_000 * 10, 16_000 * 22).unwrap()
    );
}

#[test]
fn already_owned_repair_window_never_schedules_duplicate_text() {
    for accepted in [16_000 * 30, 16_000 * 31, 16_000 * 45] {
        assert!(accurate_repair_window(0, 16_000 * 30, accepted).is_none());
    }
    assert!(accurate_repair_window(0, 0, 0).is_none());
}
