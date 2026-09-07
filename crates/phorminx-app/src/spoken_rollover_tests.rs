use super::*;

#[test]
fn instant_successful_empty_rollover_retains_overlap_and_advances_coverage() {
    let total = INSTANT_ROLLOVER_TARGET_SAMPLES;
    let terminal = VoskFinalizedSegment {
        start_sample: 0,
        end_sample: total,
        text: String::new(),
        words: Vec::new(),
    };
    let plan = plan_instant_rollover(0, 0, total, total as usize, &terminal).unwrap();
    assert_eq!(
        plan.committed_through,
        total - INSTANT_ROLLOVER_OVERLAP_SAMPLES
    );
    assert!(plan.stable_text.is_empty());
    assert_eq!(plan.replay_offset as u64, plan.committed_through);
}

#[test]
fn instant_late_first_word_rollover_preserves_whole_word_without_failing() {
    let total = INSTANT_ROLLOVER_TARGET_SAMPLES;
    for (start, end, origin) in [
        (29 * 16_000, 29 * 16_000 + 8_000, 0),
        (27 * 16_000 + 14_400, 28 * 16_000 + 8_000, 0),
        (29 * 16_000, 29 * 16_000 + 8_000, 90 * 16_000),
    ] {
        let terminal = VoskFinalizedSegment {
            start_sample: 0,
            end_sample: total,
            text: "hello".to_owned(),
            words: vec![phorminx_vosk::FinalizedWord {
                text: "hello".to_owned(),
                start_sample: start,
                end_sample: end,
            }],
        };
        let plan = plan_instant_rollover(origin, origin, origin + total, total as usize, &terminal)
            .unwrap();
        assert!(plan.stable_text.is_empty());
        assert_eq!(
            plan.committed_through,
            origin + (total - INSTANT_ROLLOVER_OVERLAP_SAMPLES).min(start)
        );
        assert_eq!(plan.replay_offset as u64, plan.committed_through - origin);
        assert!(origin + total - plan.committed_through >= INSTANT_ROLLOVER_OVERLAP_SAMPLES);
        assert!(origin + total - plan.committed_through < INSTANT_ROLLOVER_TARGET_SAMPLES);
    }
}

#[test]
#[ignore = "requires installed Vosk and the generated neutral spoken fixture"]
fn native_spoken_audio_after_initial_silence_and_ambient_noise() {
    let runtime = std::env::var_os("PHORMINX_VOSK_RUNTIME").unwrap();
    let model_path = std::env::var_os("PHORMINX_VOSK_MODEL").unwrap();
    let fixture = std::env::var_os("PHORMINX_SPOKEN_ROLLOVER_WAV").unwrap();
    let clip = phorminx_audio::read_wav(Path::new(&fixture)).unwrap();
    let model = VoskModel::load(Path::new(&runtime), Path::new(&model_path), "en").unwrap();
    let loaded = LoadedRecognizer::Instant {
        model,
        minimum_rms: 0.003,
    };
    let mut energetic_empty_endpoints = 0;
    for delay_seconds in [1, 5, 15, 29] {
        for noise_amplitude in [0.0, 0.002, 0.008] {
            let LoadedRecognizer::Instant { model, .. } = &loaded else {
                unreachable!()
            };
            let id = DictationId(910);
            let session = InstantSession {
                recognizer: model.session(WHISPER_SAMPLE_RATE).unwrap(),
                sample_rate: WHISPER_SAMPLE_RATE,
                accepted_through: 0,
                decoder_origin: 0,
                inference_time: Duration::ZERO,
                degraded: false,
                continuity_prefix: Vec::new(),
                checkpoint_count: 0,
                ownership: InstantTranscriptOwnership::default(),
                uncommitted_audio: Vec::new(),
                ownership_acknowledger: None,
            };
            // A reproducible mixture of low fan hum and broadband room noise,
            // followed by the same neutral spoken sentence in every case.
            let mut random = 0x8721_8435u32;
            let mut audio = (0..delay_seconds * WHISPER_SAMPLE_RATE as usize)
                .map(|sample| {
                    random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let noise = (random as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
                    let hum = (sample as f32 * std::f32::consts::TAU * 120.0 / 16_000.0).sin();
                    noise_amplitude * (noise * 0.7 + hum * 0.3)
                })
                .collect::<Vec<_>>();
            audio.extend_from_slice(&clip.samples[..8 * WHISPER_SAMPLE_RATE as usize]);
            audio.extend(vec![0.0; 3 * WHISPER_SAMPLE_RATE as usize]);
            let mut sessions = HashMap::from([(id, session)]);
            let (commands, incoming) = mpsc::channel();
            let (events, received) = mpsc::channel();
            let mut cursor = 0;
            let mut previous_frontier = 0;
            let mut previous_text_length = 0;
            for batch in audio.chunks(VOSK_FEED_BATCH_SAMPLES) {
                commands
                    .send(InstantAudioCommand::Audio {
                        id,
                        start_sample: cursor,
                        samples: batch.to_vec(),
                        sample_rate: WHISPER_SAMPLE_RATE,
                        dropped_samples: 0,
                    })
                    .unwrap();
                cursor += batch.len() as u64;
                drain_instant_audio(&loaded, &incoming, &mut sessions, &events);
                let reasons = received
                    .try_iter()
                    .filter_map(|event| match event {
                        WorkerEvent::InstantDegraded { reason, .. } => Some(reason),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert!(
                    !sessions[&id].degraded,
                    "initial delay={delay_seconds}s noise={noise_amplitude} failed at {}s: {reasons:?}",
                    cursor as f64 / 16_000.0
                );
                let current = &sessions[&id];
                if current.ownership.committed_through > previous_frontier {
                    let range = &audio
                        [previous_frontier as usize..current.ownership.committed_through as usize];
                    let rms = (range
                        .iter()
                        .map(|sample| f64::from(*sample).powi(2))
                        .sum::<f64>()
                        / range.len() as f64)
                        .sqrt();
                    if previous_text_length == current.ownership.text.len() && rms >= 0.003 {
                        energetic_empty_endpoints += 1;
                    }
                    previous_frontier = current.ownership.committed_through;
                    previous_text_length = current.ownership.text.len();
                }
            }
            assert!(
                sessions[&id]
                    .ownership
                    .text
                    .contains("the morning train arrived at the station"),
                "speech missing after delay={delay_seconds}s noise={noise_amplitude}: {}",
                sessions[&id].ownership.text
            );
        }
    }
    assert!(
        energetic_empty_endpoints > 0,
        "fixture must exercise a successful empty endpoint rejected by the old energy heuristic"
    );
}

#[test]
#[ignore = "requires installed Vosk and the generated neutral spoken fixture"]
fn native_spoken_audio_long_extended_release_preserves_text_after_ambient_noise() {
    let runtime = std::env::var_os("PHORMINX_VOSK_RUNTIME").unwrap();
    let model_path = std::env::var_os("PHORMINX_VOSK_MODEL").unwrap();
    let fixture = std::env::var_os("PHORMINX_SPOKEN_ROLLOVER_WAV").unwrap();
    let clip = phorminx_audio::read_wav(Path::new(&fixture)).unwrap();
    let model = VoskModel::load(Path::new(&runtime), Path::new(&model_path), "en").unwrap();
    let loaded = LoadedRecognizer::Instant {
        model,
        minimum_rms: 0.003,
    };
    let mut samples = clip.samples.clone();
    samples.extend_from_slice(&clip.samples);
    let mut random = 0x7812_9461u32;
    let noise = (0..10 * WHISPER_SAMPLE_RATE as usize)
        .map(|sample| {
            random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let broadband = (random as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
            let hum = (sample as f32 * std::f32::consts::TAU * 120.0 / 16_000.0).sin();
            0.008 * (broadband * 0.7 + hum * 0.3)
        })
        .collect::<Vec<_>>();
    assert!(
        (noise
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / noise.len() as f64)
            .sqrt()
            > 0.003
    );
    samples.extend(noise);
    let total = samples.len() as u64;
    assert!(total > 120 * u64::from(WHISPER_SAMPLE_RATE));
    for degraded in [false, true] {
        let LoadedRecognizer::Instant { model, .. } = &loaded else {
            unreachable!()
        };
        let id = DictationId(911);
        let session = InstantSession {
            recognizer: model.session(WHISPER_SAMPLE_RATE).unwrap(),
            sample_rate: WHISPER_SAMPLE_RATE,
            accepted_through: 0,
            decoder_origin: 0,
            inference_time: Duration::ZERO,
            degraded: false,
            continuity_prefix: Vec::new(),
            checkpoint_count: 0,
            ownership: InstantTranscriptOwnership::default(),
            uncommitted_audio: Vec::new(),
            ownership_acknowledger: None,
        };
        let mut sessions = HashMap::from([(id, session)]);
        let (commands, incoming) = mpsc::channel();
        let (events, received) = mpsc::channel();
        let streamed_length = samples.len() - 12 * WHISPER_SAMPLE_RATE as usize;
        let mut cursor = 0;
        for batch in samples[..streamed_length].chunks(VOSK_FEED_BATCH_SAMPLES) {
            commands
                .send(InstantAudioCommand::Audio {
                    id,
                    start_sample: cursor,
                    samples: batch.to_vec(),
                    sample_rate: WHISPER_SAMPLE_RATE,
                    dropped_samples: 0,
                })
                .unwrap();
            cursor += batch.len() as u64;
            drain_instant_audio(&loaded, &incoming, &mut sessions, &events);
            assert!(
                !sessions[&id].degraded,
                "long stream unexpectedly degraded at {cursor}"
            );
            assert!(
                sessions[&id].uncommitted_audio.len() < INSTANT_ROLLOVER_TARGET_SAMPLES as usize
            );
            assert!(
                !received
                    .try_iter()
                    .any(|event| matches!(event, WorkerEvent::InstantDegraded { .. }))
            );
        }
        let mut session = sessions.remove(&id).unwrap();
        session.degraded = degraded;
        let retained_from = session.ownership.committed_through;
        assert!(retained_from > 100 * u64::from(WHISPER_SAMPLE_RATE));
        let prefix = session.ownership.text.clone();
        let stats = FinalAudioStats {
            total_samples: total,
            peak_retained_samples: 45 * u64::from(WHISPER_SAMPLE_RATE),
            auto_stopped: false,
        };
        // This is the production extended finalizer; the range source models
        // audio already reclaimed and rejects every read before the frontier.
        let output = transcribe_extended_instant_with_source(
            model,
            Some(session),
            retained_from,
            stats,
            id,
            0.003,
            |range| {
                assert!(range.start() >= retained_from);
                assert!(range.end() <= total);
                assert!(range.end() - range.start() <= 30 * u64::from(WHISPER_SAMPLE_RATE));
                phorminx_session::AudioSpan::new(
                    range.start(),
                    samples[range.start() as usize..range.end() as usize].to_vec(),
                )
                .map_err(|error| error.to_string())
            },
        )
        .unwrap();
        let baseline = transcribe_instant_full_clip(
            model,
            &AudioClip {
                samples: samples.clone(),
                sample_rate: WHISPER_SAMPLE_RATE,
            },
            0.003,
        )
        .unwrap();
        assert!(output.text.starts_with(&prefix));
        let relative_edits = word_edit_distance(
            &baseline.text.split_whitespace().collect::<Vec<_>>(),
            &output.text.split_whitespace().collect::<Vec<_>>(),
        );
        assert!(
            relative_edits <= 10,
            "extended release differs excessively from continuous native baseline: {relative_edits}"
        );
        // Native continuous decoding of this same noisy fixture sometimes
        // tokenizes the final "coming" as "come ing". Assert ordered clauses
        // through the final word, plus a strict baseline edit budget, instead
        // of making that existing acoustic variation a false product failure.
        let anchors = [
            "the morning train arrived at the station",
            "the farmers grew apples and potatoes",
            "warm bread and vegetable soup",
            "another peaceful day was",
            "to an end",
        ];
        for text in [&output.text, &baseline.text] {
            let mut cursor = 0;
            for _ in 0..2 {
                for anchor in anchors {
                    assert_eq!(
                        text.matches(anchor).count(),
                        2,
                        "missing/repeated long clause: {anchor}"
                    );
                    cursor += text[cursor..]
                        .find(anchor)
                        .expect("long clauses changed order")
                        + anchor.len();
                }
            }
            assert!(text.ends_with("to an end"));
        }
        eprintln!(
            "extended Instant finalization: degraded={degraded} total_seconds={} retained_seconds={} words={} baseline_relative_edits={relative_edits}",
            total / 16_000,
            (total - retained_from) / 16_000,
            output.text.split_whitespace().count()
        );
    }
}

/// Opt-in integration test: the fixture is neutral synthesized speech generated
/// by scripts/Test-InstantSpokenRollover.ps1, never a user's recorded dictation.
#[test]
#[ignore = "requires installed Vosk and the generated neutral spoken fixture"]
fn native_spoken_audio_crosses_rollovers_and_preserves_release_tail() {
    check_native_spoken_stream(true);
}

#[test]
#[ignore = "requires installed Vosk and the generated neutral spoken fixture"]
fn native_spoken_audio_natural_endpoints_preserve_release_tail() {
    check_native_spoken_stream(false);
}

fn check_native_spoken_stream(force_rollover: bool) {
    let runtime = std::env::var_os("PHORMINX_VOSK_RUNTIME").unwrap();
    let model_path = std::env::var_os("PHORMINX_VOSK_MODEL").unwrap();
    let fixture = std::env::var_os("PHORMINX_SPOKEN_ROLLOVER_WAV").unwrap();
    let clip = phorminx_audio::read_wav(Path::new(&fixture)).unwrap();
    assert!(clip.duration() > Duration::from_secs(65));
    let model = VoskModel::load(Path::new(&runtime), Path::new(&model_path), "en").unwrap();
    let session = InstantSession {
        recognizer: model.session(WHISPER_SAMPLE_RATE).unwrap(),
        sample_rate: WHISPER_SAMPLE_RATE,
        accepted_through: 0,
        decoder_origin: 0,
        inference_time: Duration::ZERO,
        degraded: false,
        continuity_prefix: Vec::new(),
        checkpoint_count: 0,
        ownership: InstantTranscriptOwnership::default(),
        uncommitted_audio: Vec::new(),
        ownership_acknowledger: None,
    };
    let loaded = LoadedRecognizer::Instant {
        model,
        minimum_rms: 0.003,
    };
    let id = DictationId(909);
    let mut sessions = HashMap::from([(id, session)]);
    let (commands, incoming) = mpsc::channel();
    let (events, received) = mpsc::channel();
    // Keep real spoken audio for release so the final production path must feed
    // samples that were never seen by the streaming decoder.
    let streamed_length = clip.samples.len() - 3 * WHISPER_SAMPLE_RATE as usize;
    let mut cursor = 0;
    let mut previous_origin = 0;
    let mut rollovers = 0;
    let mut last_ack = 0;
    let mut peak_uncommitted = 0;
    for batch in clip.samples[..streamed_length].chunks(VOSK_FEED_BATCH_SAMPLES) {
        commands
            .send(InstantAudioCommand::Audio {
                id,
                start_sample: cursor,
                samples: batch.to_vec(),
                sample_rate: WHISPER_SAMPLE_RATE,
                dropped_samples: 0,
            })
            .unwrap();
        cursor += batch.len() as u64;
        drain_instant_audio(&loaded, &incoming, &mut sessions, &events);
        // This voice naturally endpoints before the production thirty-second
        // deadline. Force the same production restart/replay routine earlier
        // to make the otherwise rare continuous-speech boundary reproducible.
        let session = sessions.get_mut(&id).unwrap();
        if force_rollover
            && session.accepted_through - session.ownership.committed_through
                >= 10 * u64::from(WHISPER_SAMPLE_RATE)
        {
            let LoadedRecognizer::Instant { model, .. } = &loaded else {
                unreachable!()
            };
            force_instant_rollover(model, session, id, &events).unwrap();
            let words = session
                .ownership
                .text
                .split_whitespace()
                .collect::<Vec<_>>();
            eprintln!(
                "native rollover boundary: accepted={:.3}s committed={:.3}s prefix_tail={}",
                cursor as f64 / 16_000.0,
                session.ownership.committed_through as f64 / 16_000.0,
                words[words.len().saturating_sub(10)..].join(" ")
            );
        }
        let session = &sessions[&id];
        assert!(!session.degraded, "spoken stream unexpectedly degraded");
        assert_eq!(session.accepted_through, cursor);
        assert_eq!(
            session.uncommitted_audio.len() as u64,
            cursor - session.ownership.committed_through
        );
        peak_uncommitted = peak_uncommitted.max(session.uncommitted_audio.len());
        if session.decoder_origin != previous_origin {
            rollovers += 1;
            assert!(session.decoder_origin > previous_origin);
            assert!(
                !session.uncommitted_audio.is_empty(),
                "rollover must replay a lexical tail"
            );
            previous_origin = session.decoder_origin;
        }
        for event in received.try_iter() {
            match event {
                WorkerEvent::InstantCommitted {
                    committed_through, ..
                } => {
                    assert!(committed_through > last_ack);
                    assert!(committed_through <= cursor);
                    last_ack = committed_through;
                }
                WorkerEvent::InstantDegraded { reason, .. } => panic!("degraded: {reason}"),
                _ => panic!("unexpected worker event"),
            }
        }
    }
    if force_rollover {
        assert!(
            rollovers >= 2,
            "fixture must exercise at least two actual forced rollovers, got {rollovers}"
        );
    } else {
        assert!(
            sessions[&id].checkpoint_count >= 2,
            "fixture must exercise multiple natural endpoints"
        );
    }
    assert!((peak_uncommitted as u64) < INSTANT_ROLLOVER_TARGET_SAMPLES);
    let session = sessions.remove(&id).unwrap();
    assert_eq!(last_ack, session.ownership.committed_through);
    let committed_prefix = session.ownership.text.clone();
    let LoadedRecognizer::Instant { model, .. } = loaded else {
        unreachable!()
    };
    let result = transcribe_instant_final(&model, Some(session), &clip, id, 0.003).unwrap();
    assert!(result.text.starts_with(&committed_prefix));
    assert_eq!(result.audio_duration, clip.duration());
    let baseline = transcribe_instant_full_clip(&model, &clip, 0.003).unwrap();
    eprintln!(
        "spoken rollover QA: duration={:?} rollovers={rollovers} peak_uncommitted={peak_uncommitted}\nrolling={}\nbaseline={}",
        clip.duration(),
        result.text,
        baseline.text
    );
    // Decoder restarts can change individual word hypotheses. Compare both
    // paths with the known neutral speech and require every distinctive clause
    // once, in order, including words fed only after the final release.
    let reference = "the morning train arrived at the station and the passengers walked across the platform carrying their bags while the conductor checked the schedule and the engineer prepared the engine for another journey through the valley beyond the river where the old stone bridge stood beside a quiet village and a small school with a garden full of yellow flowers and tall green trees that provided shade for the children playing outside before their lessons began and their teacher opened the classroom windows to let the cool morning air enter the room while she arranged the books on the wooden desk and wrote a list of questions on the board about the mountains and the forests and the animals that lived nearby where the farmers grew apples and potatoes and sold fresh vegetables at the market every saturday morning when people came from neighboring towns to meet their friends and share stories about the weather and their families and their plans for the coming summer holiday near the ocean where the water was clear and the beaches were covered with soft sand and colorful shells that the children collected carefully before returning home for dinner with their grandparents who had prepared warm bread and vegetable soup for everyone to enjoy around the kitchen table as the evening light faded and the stars began to appear in the dark blue sky above the village where another peaceful day was coming to an end";
    let reference_words = reference.split_whitespace().collect::<Vec<_>>();
    let rolling_words = result.text.split_whitespace().collect::<Vec<_>>();
    let baseline_words = baseline.text.split_whitespace().collect::<Vec<_>>();
    let rolling_errors = word_edit_distance(&reference_words, &rolling_words);
    let baseline_errors = word_edit_distance(&reference_words, &baseline_words);
    let relative_errors = word_edit_distance(&baseline_words, &rolling_words);
    eprintln!(
        "native speech WER: rolling={rolling_errors}/{} baseline={baseline_errors}/{} relative_edits={relative_errors}",
        reference_words.len(),
        reference_words.len()
    );
    assert!(rolling_errors * 100 <= reference_words.len() * 8);
    assert!(rolling_errors <= baseline_errors + 3);
    assert!(relative_errors <= 5);
    let anchors = [
        "the morning train arrived at the station",
        "the engineer prepared the engine",
        "the old stone bridge stood beside a quiet village",
        "yellow flowers and tall green trees",
        "the classroom windows",
        "the books on the wooden desk",
        "the farmers grew apples and potatoes",
        "the coming summer holiday near the ocean",
        "the children collected carefully before returning home",
        "warm bread and vegetable soup",
        "the stars began to appear in the dark blue sky",
        "another peaceful day was coming to an end",
    ];
    let mut after = 0;
    for anchor in anchors {
        assert_eq!(
            result.text.matches(anchor).count(),
            1,
            "missing or repeated clause: {anchor}"
        );
        let offset = result.text[after..]
            .find(anchor)
            .expect("spoken clauses changed order");
        after += offset + anchor.len();
    }
    assert!(
        result
            .text
            .ends_with("another peaceful day was coming to an end")
    );
}

fn word_edit_distance(expected: &[&str], actual: &[&str]) -> usize {
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
