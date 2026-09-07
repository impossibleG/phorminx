use super::*;

#[test]
#[ignore = "requires installed Whisper model"]
fn native_accurate_speech_ambient_no_words_does_not_fill_the_buffer() {
    use phorminx_app::incremental::{ChunkPlan, TimeRange};
    let model = std::env::var_os("PHORMINX_WHISPER_MODEL").unwrap();
    let recognizer =
        WhisperRecognizer::load_with_backend(Path::new(&model), WhisperBackendPreference::Auto)
            .unwrap();
    let mut accumulator = PartialAccumulator {
        text: "previously recognized words".to_owned(),
        ..PartialAccumulator::default()
    };
    let abort = Arc::new(AtomicBool::new(false));
    let options = TranscriptionOptions {
        language: Some("en"),
        thread_count: None,
        audio_context: None,
    };
    for sequence in 0..7 {
        let start = Duration::from_secs(u64::from(sequence) * 12);
        let end = start + Duration::from_secs(12);
        // A continuous room/electrical tone is energetic but is not speech.
        let clip = AudioClip::new(
            (0..16_000 * 12)
                .map(|i| 0.02 * (i as f32 * std::f32::consts::TAU * 120.0 / 16_000.0).sin())
                .collect(),
            16_000,
        )
        .unwrap();
        repair_accurate_chunk(
            &recognizer,
            &mut accumulator,
            ChunkPlan {
                id: DictationId(913),
                sequence,
                range: TimeRange { start, end },
                stable_end: end,
                start_overlap: None,
                boundary: BoundaryKind::Forced,
            },
            &clip,
            &options,
            &abort,
        )
        .unwrap();
        assert_eq!(accumulator.accepted_through, end);
        assert!(accumulator.text.starts_with("previously recognized words"));
    }
    eprintln!("accurate_ambient audio_s=84 committed_s=84");
}

#[test]
#[ignore = "requires installed Whisper model and neutral synthesized spoken fixture"]
fn native_accurate_speech_continues_beyond_45_seconds_after_startup_silence() {
    let model = std::env::var_os("PHORMINX_WHISPER_MODEL").unwrap();
    let fixture = std::env::var_os("PHORMINX_SPOKEN_ROLLOVER_WAV").unwrap();
    let spoken = phorminx_audio::read_wav(Path::new(&fixture)).unwrap();
    let mut samples = vec![0.0; WHISPER_SAMPLE_RATE as usize * 5];
    for _ in 0..3 {
        samples.extend_from_slice(&spoken.samples);
    }
    samples.extend(std::iter::repeat_n(0.0, WHISPER_SAMPLE_RATE as usize * 3));
    assert!(samples.len() > WHISPER_SAMPLE_RATE as usize * 65);
    let recognizer =
        WhisperRecognizer::load_with_backend(Path::new(&model), WhisperBackendPreference::Auto)
            .unwrap();
    let mut planner = IncrementalPlanner::default();
    let id = DictationId(911);
    planner.start(id);
    let mut sessions = HashMap::new();
    let abort = Arc::new(AtomicBool::new(false));
    let mut acknowledged = 0u64;
    let mut captured = 0u64;
    let mut compute_total = Duration::ZERO;
    let mut peak_backlog = 0u64;
    let mut retried_failure = false;
    while captured < samples.len() as u64 {
        captured = (captured + 3200).min(samples.len() as u64);
        let time = canonical_duration(captured);
        if !planner.needs_probe(id, time) {
            continue;
        }
        let probe = &samples[captured.saturating_sub(8000) as usize..captured as usize];
        let silence = probe.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>()
            / (probe.len() as f64)
            < 0.003f64.powi(2);
        if let Some(mut plan) = planner.observe(id, time, silence) {
            let start = canonical_sample(plan.range.start);
            let end = canonical_sample(plan.range.end);
            let clip = AudioClip {
                samples: samples[start as usize..end as usize].to_vec(),
                sample_rate: WHISPER_SAMPLE_RATE,
            };
            if !retried_failure
                && plan.boundary == BoundaryKind::Forced
                && sessions
                    .get(&id)
                    .is_some_and(|s: &PartialAccumulator| !s.text.is_empty())
            {
                let before = sessions.get(&id).unwrap();
                let owned_text = before.text.clone();
                let owned_frontier = before.accepted_through;
                abort.store(true, Ordering::Release);
                let failed = process_partial_transcription(
                    &recognizer,
                    &mut sessions,
                    plan,
                    clip.clone(),
                    "en",
                    0.003,
                    &abort,
                );
                assert!(matches!(
                    failed,
                    WorkerEvent::PartialCompleted {
                        succeeded: false,
                        committed_through: None,
                        ..
                    }
                ));
                assert_eq!(sessions[&id].text, owned_text);
                assert_eq!(sessions[&id].accepted_through, owned_frontier);
                planner.partial_completed(id, plan.sequence, false, None);
                abort.store(false, Ordering::Release);
                let retry = planner
                    .observe(id, time + Duration::from_millis(200), false)
                    .unwrap();
                assert_eq!(retry.sequence, plan.sequence);
                assert_eq!(retry.range, plan.range);
                plan = retry;
                retried_failure = true;
            }
            let event = process_partial_transcription(
                &recognizer,
                &mut sessions,
                plan,
                clip,
                "en",
                0.003,
                &abort,
            );
            if let WorkerEvent::PartialCompleted {
                succeeded,
                repair_from,
                committed_through,
                compute_time,
                ..
            } = event
            {
                compute_total = compute_total.saturating_add(compute_time);
                // Model the microphone progressing while this synchronous
                // native call runs, as it does on the production worker.
                captured = captured
                    .saturating_add(canonical_sample(compute_time))
                    .min(samples.len() as u64);
                peak_backlog = peak_backlog.max(captured.saturating_sub(acknowledged));
                let session = sessions.get(&id).unwrap();
                eprintln!(
                    "captured={:.1} range={:.1}-{:.1} committed={:.1} success={} compute={:?} degraded={:?}",
                    time.as_secs_f64(),
                    plan.range.start.as_secs_f64(),
                    plan.range.end.as_secs_f64(),
                    session.accepted_through.as_secs_f64(),
                    succeeded,
                    compute_time,
                    session.degraded
                );
                assert!(succeeded, "live accurate decode must continue");
                acknowledged = committed_through.unwrap_or(acknowledged);
                planner.partial_completed(
                    id,
                    plan.sequence,
                    succeeded,
                    repair_from.map(canonical_duration),
                );
            }
        }
        assert!(
            captured.saturating_sub(acknowledged) < 43 * u64::from(WHISPER_SAMPLE_RATE),
            "uncommitted audio would fill capture buffer"
        );
    }
    let session = sessions.get(&id).unwrap();
    assert!(retried_failure);
    assert!(
        canonical_duration(samples.len() as u64).saturating_sub(session.accepted_through)
            < Duration::from_secs(30)
    );
    let retained_from =
        canonical_sample(session.accepted_through).saturating_sub(ACCURATE_REPAIR_OVERLAP_SAMPLES);
    let transcript = transcribe_extended_accurate_with_source(
        &recognizer,
        sessions.remove(&id),
        FinalAudioStats {
            total_samples: samples.len() as u64,
            peak_retained_samples: peak_backlog,
            auto_stopped: false,
        },
        retained_from,
        "en",
        id,
        |range| {
            assert!(range.start() >= retained_from);
            AudioClip::new(
                samples[range.start() as usize..range.end() as usize].to_vec(),
                WHISPER_SAMPLE_RATE,
            )
            .map_err(|error| error.to_string())
        },
    )
    .unwrap();
    let text = transcript.text.to_lowercase();
    let baseline = recognizer
        .transcribe(
            &spoken,
            &TranscriptionOptions {
                language: Some("en"),
                thread_count: None,
                audio_context: None,
            },
        )
        .unwrap();
    let expected = std::env::var("PHORMINX_ACCURATE_REFERENCE").unwrap();
    let reference_words = normalized_word_ranges(&format!("{expected} {expected} {expected}"))
        .into_iter()
        .map(|(word, _)| word)
        .collect::<Vec<_>>();
    let baseline_text = format!("{} {} {}", baseline.text, baseline.text, baseline.text);
    let baseline_words = normalized_word_ranges(&baseline_text)
        .into_iter()
        .map(|(word, _)| word)
        .collect::<Vec<_>>();
    let actual_words = normalized_word_ranges(&text)
        .into_iter()
        .map(|(word, _)| word)
        .collect::<Vec<_>>();
    let baseline_error = accurate_edit_distance(&reference_words, &baseline_words);
    let rolling_error = accurate_edit_distance(&reference_words, &actual_words);
    eprintln!(
        "accurate_wer reference_words={} rolling_words={} baseline_words={} rolling_edits={} baseline_edits={}",
        reference_words.len(),
        actual_words.len(),
        baseline_words.len(),
        rolling_error,
        baseline_error
    );
    assert!(
        rolling_error <= baseline_error + reference_words.len() / 10,
        "streaming introduced excessive recognition errors or duplicates"
    );
    eprintln!(
        "accurate_quality train_count={} stars_count={} words={}",
        text.matches("train").count(),
        text.matches("stars").count(),
        text.split_whitespace().count()
    );
    assert!(
        text.matches("train").count() >= 3,
        "one of the repeated passages disappeared"
    );
    assert!(
        text.matches("stars").count() >= 3,
        "one of the passage endings disappeared"
    );
    assert!(
        text.split_whitespace().count() < 2000,
        "bounded retry produced excessive duplication"
    );
    let anchors = [
        "train",
        "platform",
        "engine",
        "village",
        "school",
        "flowers",
        "teacher",
        "mountains",
        "farmers",
        "market",
        "ocean",
        "grandparents",
        "soup",
        "stars",
    ];
    let mut cursor = 0;
    for passage in 0..3 {
        for anchor in anchors {
            let relative = text[cursor..].find(anchor).unwrap_or_else(|| {
                panic!("recognized speech missing ordered clause {anchor} in passage {passage}")
            });
            cursor += relative + anchor.len();
        }
    }
    eprintln!(
        "accurate_soak audio_s={:.2} compute_s={:.2} peak_backlog_s={:.2} words={} backend={}",
        canonical_duration(samples.len() as u64).as_secs_f64(),
        compute_total.as_secs_f64(),
        canonical_duration(peak_backlog).as_secs_f64(),
        text.split_whitespace().count(),
        recognizer.readiness().backend.as_str()
    );
    assert!(peak_backlog < 43 * u64::from(WHISPER_SAMPLE_RATE));
}

fn accurate_edit_distance(expected: &[String], actual: &[String]) -> usize {
    let mut previous = (0..=actual.len()).collect::<Vec<_>>();
    for (row, word) in expected.iter().enumerate() {
        let mut current = vec![row + 1];
        for (column, other) in actual.iter().enumerate() {
            current.push(
                (previous[column] + usize::from(word != other))
                    .min(previous[column + 1] + 1)
                    .min(current[column] + 1),
            );
        }
        previous = current;
    }
    previous[actual.len()]
}
