use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{
    BackendKind, BenchmarkEvidence, BenchmarkProtocol, BenchmarkSampleSummary, ContentFreeId,
    ContentionCondition, EngineKind, Language, ModelClass, Sha256Digest, ThermalCondition,
    TrustedCandidateIdentity,
};

/// Exact, host-trusted identity of a benchmarkable recognition configuration.
/// It deliberately has no deserializer: persisted evidence cannot mint runtime authority.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BenchmarkCandidate {
    candidate_id: ContentFreeId,
    engine: EngineKind,
    backend: BackendKind,
    model_class: ModelClass,
    model_digest: Sha256Digest,
    supported_languages: BTreeSet<Language>,
    pt_brazil_instant_certified: bool,
}

impl BenchmarkCandidate {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        candidate_id: ContentFreeId,
        engine: EngineKind,
        backend: BackendKind,
        model_class: ModelClass,
        model_digest: Sha256Digest,
        supported_languages: impl IntoIterator<Item = Language>,
        pt_brazil_instant_certified: bool,
    ) -> Self {
        Self {
            candidate_id,
            engine,
            backend,
            model_class,
            model_digest,
            supported_languages: supported_languages.into_iter().collect(),
            pt_brazil_instant_certified,
        }
    }

    #[must_use]
    pub fn candidate_id(&self) -> &ContentFreeId {
        &self.candidate_id
    }

    #[must_use]
    pub const fn engine(&self) -> EngineKind {
        self.engine
    }

    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    #[must_use]
    pub const fn model_class(&self) -> ModelClass {
        self.model_class
    }

    #[must_use]
    pub fn model_digest(&self) -> &Sha256Digest {
        &self.model_digest
    }

    #[must_use]
    pub fn supports(&self, language: Language) -> bool {
        self.supported_languages.contains(&language)
    }

    #[must_use]
    pub const fn pt_brazil_instant_certified(&self) -> bool {
        self.pt_brazil_instant_certified
    }

    #[must_use]
    pub fn trusted_identity(&self) -> TrustedCandidateIdentity {
        TrustedCandidateIdentity::new(
            self.candidate_id.clone(),
            self.engine,
            self.backend,
            self.model_class,
            self.model_digest.clone(),
            self.supported_languages.iter().copied(),
            self.pt_brazil_instant_certified,
        )
    }

    #[must_use]
    pub fn matches_evidence(&self, evidence: &BenchmarkEvidence) -> bool {
        self.candidate_id == evidence.candidate_id
            && self.engine == evidence.engine
            && self.backend == evidence.backend
            && self.model_class == evidence.model_class
            && self.model_digest == evidence.model_digest
            && self.supported_languages == evidence.supported_languages
            && self.pt_brazil_instant_certified == evidence.pt_brazil_instant_certified
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchmarkContext {
    pub protocol_id: ContentFreeId,
    pub build_id: ContentFreeId,
    pub device_id: ContentFreeId,
    pub driver_id: ContentFreeId,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CalibrationKind {
    Speech,
    Silence,
}

/// A bundled prompt. Debug output is redacted and this type is not serializable.
#[derive(Clone, Eq, PartialEq)]
pub struct CalibrationCase {
    case_id: ContentFreeId,
    language: Language,
    kind: CalibrationKind,
    reference: &'static str,
    protected_tokens: &'static [&'static str],
    reference_digest: Sha256Digest,
}

impl fmt::Debug for CalibrationCase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CalibrationCase")
            .field("case_id", &self.case_id)
            .field("language", &self.language)
            .field("kind", &self.kind)
            .field("reference", &"[REDACTED]")
            .field("protected_tokens", &self.protected_tokens.len())
            .field("reference_digest", &self.reference_digest)
            .finish()
    }
}

impl CalibrationCase {
    fn new(
        id: &'static str,
        language: Language,
        kind: CalibrationKind,
        reference: &'static str,
        protected_tokens: &'static [&'static str],
    ) -> Self {
        let digest = Sha256::digest(reference.as_bytes());
        let digest = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Self {
            case_id: ContentFreeId::new(id).expect("bundled case IDs are valid"),
            language,
            kind,
            reference,
            protected_tokens,
            reference_digest: Sha256Digest::new(digest).expect("SHA-256 output is valid"),
        }
    }

    #[must_use]
    pub fn case_id(&self) -> &ContentFreeId {
        &self.case_id
    }

    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    #[must_use]
    pub const fn kind(&self) -> CalibrationKind {
        self.kind
    }

    #[must_use]
    pub fn reference_digest(&self) -> &Sha256Digest {
        &self.reference_digest
    }

    /// The host lends content directly to the recognizer/scorer. It must not log or persist it.
    #[must_use]
    pub const fn ephemeral_content(&self) -> EphemeralCalibration<'static> {
        EphemeralCalibration {
            reference: self.reference,
            protected_tokens: self.protected_tokens,
        }
    }
}

pub struct EphemeralCalibration<'a> {
    reference: &'a str,
    protected_tokens: &'a [&'a str],
}

impl fmt::Debug for EphemeralCalibration<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EphemeralCalibration([REDACTED])")
    }
}

impl<'a> EphemeralCalibration<'a> {
    #[must_use]
    pub const fn reference(&self) -> &'a str {
        self.reference
    }

    #[must_use]
    pub const fn protected_tokens(&self) -> &'a [&'a str] {
        self.protected_tokens
    }
}

/// Versioned, compile-time bundled bilingual calibration material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalibrationCorpus {
    protocol: BenchmarkProtocol,
    cases: Vec<CalibrationCase>,
}

impl CalibrationCorpus {
    pub const PROTOCOL_ID: &'static str = "phorminx-perf-v1";

    #[must_use]
    pub fn pinned_v1() -> Self {
        let cases = vec![
            CalibrationCase::new(
                "en-speech-01",
                Language::English,
                CalibrationKind::Speech,
                "Phorminx keeps Ctrl Alt Space responsive during daily dictation.",
                &["Phorminx", "Ctrl Alt Space"],
            ),
            CalibrationCase::new(
                "en-speech-02",
                Language::English,
                CalibrationKind::Speech,
                "Send the release notes to qa@example.com at nine forty-five.",
                &["qa@example.com", "nine forty-five"],
            ),
            CalibrationCase::new(
                "en-speech-03",
                Language::English,
                CalibrationKind::Speech,
                "The local model formats names, numbers, and punctuation without changing meaning.",
                &["local model"],
            ),
            CalibrationCase::new(
                "en-silence-01",
                Language::English,
                CalibrationKind::Silence,
                "",
                &[],
            ),
            CalibrationCase::new(
                "ptbr-speech-01",
                Language::PortugueseBrazil,
                CalibrationKind::Speech,
                "O Phorminx mantém Ctrl Alt Espaço responsivo durante o ditado diário.",
                &["Phorminx", "Ctrl Alt Espaço"],
            ),
            CalibrationCase::new(
                "ptbr-speech-02",
                Language::PortugueseBrazil,
                CalibrationKind::Speech,
                "Envie as notas da versão para qa@example.com às nove e quarenta e cinco.",
                &["qa@example.com", "nove e quarenta e cinco"],
            ),
            CalibrationCase::new(
                "ptbr-speech-03",
                Language::PortugueseBrazil,
                CalibrationKind::Speech,
                "O modelo local formata nomes, números e pontuação sem mudar o sentido.",
                &["modelo local"],
            ),
            CalibrationCase::new(
                "ptbr-silence-01",
                Language::PortugueseBrazil,
                CalibrationKind::Silence,
                "",
                &[],
            ),
        ];
        Self {
            protocol: BenchmarkProtocol {
                protocol_id: ContentFreeId::new(Self::PROTOCOL_ID)
                    .expect("bundled protocol ID is valid"),
                minimum_speech_samples: 3,
                minimum_silence_samples: 1,
            },
            cases,
        }
    }

    #[must_use]
    pub fn protocol(&self) -> &BenchmarkProtocol {
        &self.protocol
    }

    pub fn cases_for(&self, language: Language) -> impl Iterator<Item = &CalibrationCase> {
        self.cases
            .iter()
            .filter(move |case| case.language == language)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QualityCounts {
    pub reference_words: u32,
    pub word_errors: u32,
    pub reference_characters: u32,
    pub character_errors: u32,
    pub protected_tokens: u32,
    pub protected_tokens_exact: u32,
    pub hallucinated_tokens: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchmarkObservation {
    pub case_id: ContentFreeId,
    pub cold_load: bool,
    pub load_latency: Duration,
    pub release_latency: Duration,
    pub inference_time: Duration,
    pub audio_duration: Duration,
    pub peak_working_set_mib: u32,
    pub available_memory_mib: u32,
    pub fallback_count: u32,
    pub thermal_condition: ThermalCondition,
    pub contention_condition: ContentionCondition,
    pub quality: QualityCounts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AggregationError {
    #[error("candidate does not support the requested language")]
    UnsupportedLanguage,
    #[error("benchmark protocol identity does not match the bundled corpus")]
    ProtocolMismatch,
    #[error("the observation set is incomplete, duplicated, or contains an unknown case")]
    InvalidCaseSet,
    #[error("a measurement is outside safe numeric bounds")]
    InvalidMeasurement,
    #[error("a measurement was thermally unstable")]
    ThermalBias,
    #[error("a measurement was taken under system contention")]
    SystemContention,
}

#[must_use]
pub fn score_transcript(calibration: &EphemeralCalibration<'_>, transcript: &str) -> QualityCounts {
    let reference_words = tokenize_words(calibration.reference()).collect::<Vec<_>>();
    let transcript_words = tokenize_words(transcript).collect::<Vec<_>>();
    let reference_characters = normalize_characters(calibration.reference());
    let transcript_characters = normalize_characters(transcript);
    let protected_tokens = calibration.protected_tokens();
    let protected_tokens_exact = protected_tokens
        .iter()
        .filter(|token| {
            let expected = bounded_occurrences(calibration.reference(), token);
            expected > 0 && bounded_occurrences(transcript, token) == expected
        })
        .count();
    let word_edits = edit_counts(&reference_words, &transcript_words);
    let character_edits = edit_counts(&reference_characters, &transcript_characters);
    let silence = calibration.reference().is_empty();

    QualityCounts {
        reference_words: u32::try_from(reference_words.len()).unwrap_or(u32::MAX),
        word_errors: if silence {
            0
        } else {
            u32::try_from(word_edits.distance).unwrap_or(u32::MAX)
        },
        reference_characters: u32::try_from(reference_characters.len()).unwrap_or(u32::MAX),
        character_errors: if silence {
            0
        } else {
            u32::try_from(character_edits.distance).unwrap_or(u32::MAX)
        },
        protected_tokens: u32::try_from(protected_tokens.len()).unwrap_or(u32::MAX),
        protected_tokens_exact: u32::try_from(protected_tokens_exact).unwrap_or(u32::MAX),
        hallucinated_tokens: if silence {
            u32::try_from(transcript_words.len()).unwrap_or(u32::MAX)
        } else {
            u32::try_from(
                word_edits
                    .insertions
                    .max(usize::from(character_edits.insertions > 0)),
            )
            .unwrap_or(u32::MAX)
        },
    }
}

pub fn aggregate_evidence(
    candidate: &BenchmarkCandidate,
    context: &BenchmarkContext,
    corpus: &CalibrationCorpus,
    language: Language,
    observations: &[BenchmarkObservation],
) -> Result<BenchmarkEvidence, AggregationError> {
    if !candidate.supports(language) {
        return Err(AggregationError::UnsupportedLanguage);
    }
    if context.protocol_id != corpus.protocol.protocol_id {
        return Err(AggregationError::ProtocolMismatch);
    }

    let expected = corpus
        .cases_for(language)
        .map(|case| (case.case_id.clone(), case.kind))
        .collect::<BTreeMap<_, _>>();
    if observations.len() != expected.len() {
        return Err(AggregationError::InvalidCaseSet);
    }
    let mut observed_ids = BTreeSet::new();
    let mut speech_samples = 0_u32;
    let mut silence_samples = 0_u32;
    let mut cold_load = Vec::new();
    let mut warm_load = Vec::new();
    let mut release = Vec::new();
    let mut realtime_factors = Vec::new();
    let mut peak_working_set_mib = 0_u32;
    let mut available_memory_mib = u32::MAX;
    let mut fallback_count = 0_u32;
    let mut quality = QualityCounts {
        reference_words: 0,
        word_errors: 0,
        reference_characters: 0,
        character_errors: 0,
        protected_tokens: 0,
        protected_tokens_exact: 0,
        hallucinated_tokens: 0,
    };

    for observation in observations {
        let Some(kind) = expected.get(&observation.case_id).copied() else {
            return Err(AggregationError::InvalidCaseSet);
        };
        if !observed_ids.insert(observation.case_id.clone()) {
            return Err(AggregationError::InvalidCaseSet);
        }
        if observation.thermal_condition != ThermalCondition::Nominal {
            return Err(AggregationError::ThermalBias);
        }
        if observation.contention_condition != ContentionCondition::Idle {
            return Err(AggregationError::SystemContention);
        }
        if observation.audio_duration.is_zero()
            || observation.available_memory_mib == 0
            || observation.quality.protected_tokens_exact > observation.quality.protected_tokens
            || (kind == CalibrationKind::Speech
                && (observation.quality.reference_words == 0
                    || observation.quality.reference_characters == 0))
            || (kind == CalibrationKind::Silence
                && (observation.quality.reference_words != 0
                    || observation.quality.reference_characters != 0
                    || observation.quality.word_errors != 0
                    || observation.quality.character_errors != 0))
        {
            return Err(AggregationError::InvalidMeasurement);
        }

        match kind {
            CalibrationKind::Speech => speech_samples = speech_samples.saturating_add(1),
            CalibrationKind::Silence => silence_samples = silence_samples.saturating_add(1),
        }
        let load_ms = duration_ms(observation.load_latency)?;
        if observation.cold_load {
            cold_load.push(load_ms);
        } else {
            warm_load.push(load_ms);
        }
        release.push(duration_ms(observation.release_latency)?);
        let inference_micros = observation.inference_time.as_micros();
        let audio_micros = observation.audio_duration.as_micros();
        let rtf = inference_micros
            .saturating_mul(1_000)
            .checked_div(audio_micros)
            .unwrap_or(u128::MAX);
        realtime_factors
            .push(u64::try_from(rtf).map_err(|_| AggregationError::InvalidMeasurement)?);
        peak_working_set_mib = peak_working_set_mib.max(observation.peak_working_set_mib);
        available_memory_mib = available_memory_mib.min(observation.available_memory_mib);
        fallback_count = fallback_count.saturating_add(observation.fallback_count);
        quality.reference_words = quality
            .reference_words
            .checked_add(observation.quality.reference_words)
            .ok_or(AggregationError::InvalidMeasurement)?;
        quality.word_errors = quality
            .word_errors
            .checked_add(observation.quality.word_errors)
            .ok_or(AggregationError::InvalidMeasurement)?;
        quality.reference_characters = quality
            .reference_characters
            .checked_add(observation.quality.reference_characters)
            .ok_or(AggregationError::InvalidMeasurement)?;
        quality.character_errors = quality
            .character_errors
            .checked_add(observation.quality.character_errors)
            .ok_or(AggregationError::InvalidMeasurement)?;
        quality.protected_tokens = quality
            .protected_tokens
            .checked_add(observation.quality.protected_tokens)
            .ok_or(AggregationError::InvalidMeasurement)?;
        quality.protected_tokens_exact = quality
            .protected_tokens_exact
            .checked_add(observation.quality.protected_tokens_exact)
            .ok_or(AggregationError::InvalidMeasurement)?;
        quality.hallucinated_tokens = quality
            .hallucinated_tokens
            .checked_add(observation.quality.hallucinated_tokens)
            .ok_or(AggregationError::InvalidMeasurement)?;
    }

    if speech_samples < corpus.protocol.minimum_speech_samples
        || silence_samples < corpus.protocol.minimum_silence_samples
        || cold_load.len() != 1
        || warm_load.len() < 2
    {
        return Err(AggregationError::InvalidCaseSet);
    }

    release.sort_unstable();
    cold_load.sort_unstable();
    warm_load.sort_unstable();
    realtime_factors.sort_unstable();
    let release_p50 = percentile(&release, 50).ok_or(AggregationError::InvalidCaseSet)?;
    let release_p95 = percentile(&release, 95).ok_or(AggregationError::InvalidCaseSet)?;
    let mut deviations = release
        .iter()
        .map(|value| value.abs_diff(release_p50))
        .collect::<Vec<_>>();
    deviations.sort_unstable();

    let measurements = BenchmarkSampleSummary {
        speech_samples,
        silence_samples,
        run_count: u32::try_from(observations.len())
            .map_err(|_| AggregationError::InvalidMeasurement)?,
        cold_load_ms: cold_load[0],
        warm_load_ms: percentile(&warm_load, 50).ok_or(AggregationError::InvalidCaseSet)?,
        release_p50_ms: release_p50,
        release_p95_ms: release_p95,
        release_dispersion_ms: percentile(&deviations, 50)
            .ok_or(AggregationError::InvalidCaseSet)?,
        confidence_per_mille: confidence(speech_samples, silence_samples),
        realtime_factor_milli: u32::try_from(
            percentile(&realtime_factors, 95).ok_or(AggregationError::InvalidCaseSet)?,
        )
        .map_err(|_| AggregationError::InvalidMeasurement)?,
        word_error_per_mille: ratio_per_mille(quality.word_errors, quality.reference_words)?,
        character_error_per_mille: ratio_per_mille(
            quality.character_errors,
            quality.reference_characters,
        )?,
        protected_token_exact_per_mille: if quality.protected_tokens == 0 {
            1_000
        } else {
            ratio_per_mille(quality.protected_tokens_exact, quality.protected_tokens)?
        },
        hallucination_per_mille: if quality.hallucinated_tokens == 0 {
            0
        } else {
            1_000
        },
        peak_working_set_mib,
        available_memory_mib,
        fallback_count,
        thermal_condition: ThermalCondition::Nominal,
        contention_condition: ContentionCondition::Idle,
    };

    Ok(BenchmarkEvidence {
        schema_version: BenchmarkEvidence::SCHEMA_VERSION,
        protocol_id: context.protocol_id.clone(),
        build_id: context.build_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        device_id: context.device_id.clone(),
        driver_id: context.driver_id.clone(),
        engine: candidate.engine,
        backend: candidate.backend,
        model_class: candidate.model_class,
        model_digest: candidate.model_digest.clone(),
        measured_language: language,
        supported_languages: candidate.supported_languages.clone(),
        pt_brazil_instant_certified: candidate.pt_brazil_instant_certified,
        loaded: true,
        measurements,
    })
}

fn duration_ms(duration: Duration) -> Result<u64, AggregationError> {
    u64::try_from(duration.as_millis()).map_err(|_| AggregationError::InvalidMeasurement)
}

fn percentile(values: &[u64], percent: usize) -> Option<u64> {
    if values.is_empty() || percent == 0 || percent > 100 {
        return None;
    }
    let rank = values.len().saturating_mul(percent).div_ceil(100);
    values.get(rank.saturating_sub(1)).copied()
}

fn ratio_per_mille(numerator: u32, denominator: u32) -> Result<u16, AggregationError> {
    if denominator == 0 {
        return Err(AggregationError::InvalidMeasurement);
    }
    let value = u64::from(numerator)
        .saturating_mul(1_000)
        .checked_div(u64::from(denominator))
        .ok_or(AggregationError::InvalidMeasurement)?
        .min(1_000);
    u16::try_from(value).map_err(|_| AggregationError::InvalidMeasurement)
}

fn confidence(speech_samples: u32, silence_samples: u32) -> u16 {
    let extra = speech_samples
        .saturating_sub(3)
        .saturating_add(silence_samples.saturating_sub(1))
        .saturating_mul(25);
    let bounded = 900_u32.saturating_add(extra).min(1_000);
    bounded as u16
}

fn tokenize_words(value: &str) -> impl Iterator<Item = String> + '_ {
    value.split_whitespace().map(|word| {
        word.trim_matches(|character: char| !character.is_alphanumeric() && character != '@')
            .to_lowercase()
    })
}

fn normalize_characters(value: &str) -> Vec<char> {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct EditCounts {
    distance: usize,
    insertions: usize,
}

fn edit_counts<T: Eq>(left: &[T], right: &[T]) -> EditCounts {
    let mut previous = (0..=right.len())
        .map(|insertions| EditCounts {
            distance: insertions,
            insertions,
        })
        .collect::<Vec<_>>();
    let mut current = vec![EditCounts::default(); right.len() + 1];
    for (left_index, left_value) in left.iter().enumerate() {
        current[0] = EditCounts {
            distance: left_index + 1,
            insertions: 0,
        };
        for (right_index, right_value) in right.iter().enumerate() {
            let mut substitution = previous[right_index];
            substitution.distance += usize::from(left_value != right_value);
            let mut insertion = current[right_index];
            insertion.distance += 1;
            insertion.insertions += 1;
            let mut deletion = previous[right_index + 1];
            deletion.distance += 1;
            current[right_index + 1] = [substitution, insertion, deletion]
                .into_iter()
                .min_by_key(|score| (score.distance, std::cmp::Reverse(score.insertions)))
                .expect("three edit candidates are present");
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn bounded_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .match_indices(needle)
        .filter(|(start, matched)| {
            let before = haystack[..*start].chars().next_back();
            let end = start.saturating_add(matched.len());
            let after = haystack[end..].chars().next();
            before.is_none_or(|character| !character.is_alphanumeric())
                && after.is_none_or(|character| !character.is_alphanumeric())
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ContentFreeId {
        ContentFreeId::new(value).unwrap()
    }

    fn candidate(language: Language) -> BenchmarkCandidate {
        BenchmarkCandidate::new(
            id("accurate-base-vulkan"),
            EngineKind::Accurate,
            BackendKind::Vulkan,
            ModelClass::Base,
            Sha256Digest::new("a".repeat(64)).unwrap(),
            [language],
            false,
        )
    }

    fn context() -> BenchmarkContext {
        BenchmarkContext {
            protocol_id: id(CalibrationCorpus::PROTOCOL_ID),
            build_id: id("build-1"),
            device_id: id("device-1"),
            driver_id: id("driver-1"),
        }
    }

    fn perfect_observations(
        corpus: &CalibrationCorpus,
        language: Language,
    ) -> Vec<BenchmarkObservation> {
        corpus
            .cases_for(language)
            .enumerate()
            .map(|(index, case)| BenchmarkObservation {
                case_id: case.case_id.clone(),
                cold_load: index == 0,
                load_latency: Duration::from_millis(if index == 0 { 400 } else { 40 }),
                release_latency: Duration::from_millis([100, 200, 300, 400][index]),
                inference_time: Duration::from_millis(100),
                audio_duration: Duration::from_secs(2),
                peak_working_set_mib: 500,
                available_memory_mib: 4_000,
                fallback_count: 0,
                thermal_condition: ThermalCondition::Nominal,
                contention_condition: ContentionCondition::Idle,
                quality: score_transcript(&case.ephemeral_content(), case.reference),
            })
            .collect()
    }

    #[test]
    fn bundled_corpus_is_bilingual_bounded_and_redacted_in_debug() {
        let corpus = CalibrationCorpus::pinned_v1();
        for language in [Language::English, Language::PortugueseBrazil] {
            let cases = corpus.cases_for(language).collect::<Vec<_>>();
            assert_eq!(cases.len(), 4);
            assert_eq!(
                cases
                    .iter()
                    .filter(|case| case.kind == CalibrationKind::Speech)
                    .count(),
                3
            );
            assert_eq!(
                cases
                    .iter()
                    .filter(|case| case.kind == CalibrationKind::Silence)
                    .count(),
                1
            );
        }
        let debug = format!("{corpus:?}");
        assert!(!debug.contains("release notes"));
        assert!(!debug.contains("notas da versão"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn scoring_measures_edits_protected_tokens_and_silence_hallucination() {
        let corpus = CalibrationCorpus::pinned_v1();
        let case = corpus.cases_for(Language::English).next().unwrap();
        let score = score_transcript(&case.ephemeral_content(), "something unrelated");
        assert!(score.word_errors > 0);
        assert_eq!(score.protected_tokens_exact, 0);

        let silence = corpus
            .cases_for(Language::English)
            .find(|case| case.kind == CalibrationKind::Silence)
            .unwrap();
        assert_eq!(
            score_transcript(&silence.ephemeral_content(), "invented words").hallucinated_tokens,
            2
        );

        let exact = score_transcript(
            &case.ephemeral_content(),
            case.ephemeral_content().reference(),
        );
        let appended = score_transcript(
            &case.ephemeral_content(),
            "Phorminxx keeps Ctrl Alt Spacex responsive during daily dictation.",
        );
        let duplicated = score_transcript(
            &case.ephemeral_content(),
            "Phorminx Phorminx keeps Ctrl Alt Space responsive during daily dictation.",
        );
        assert_eq!(exact.protected_tokens_exact, exact.protected_tokens);
        assert_eq!(appended.protected_tokens_exact, 0);
        assert!(appended.hallucinated_tokens > 0);
        assert!(duplicated.protected_tokens_exact < duplicated.protected_tokens);
    }

    #[test]
    fn insertions_are_hallucinations_and_excessive_error_rates_saturate() {
        let corpus = CalibrationCorpus::pinned_v1();
        let case = corpus.cases_for(Language::English).next().unwrap();
        let scored = score_transcript(
            &case.ephemeral_content(),
            "Phorminx keeps Ctrl Alt Space responsive during daily dictation plus many invented words afterward",
        );
        assert!(scored.hallucinated_tokens > 0);

        let mut observations = perfect_observations(&corpus, Language::English);
        observations[0].quality.word_errors =
            observations[0].quality.reference_words.saturating_mul(10);
        observations[0].quality.character_errors = observations[0]
            .quality
            .reference_characters
            .saturating_mul(10);
        observations[0].quality.hallucinated_tokens = 3;
        let evidence = aggregate_evidence(
            &candidate(Language::English),
            &context(),
            &corpus,
            Language::English,
            &observations,
        )
        .unwrap();
        assert_eq!(evidence.measurements.word_error_per_mille, 1_000);
        assert_eq!(evidence.measurements.character_error_per_mille, 1_000);
        assert_eq!(evidence.measurements.hallucination_per_mille, 1_000);
    }

    #[test]
    fn aggregation_uses_nearest_rank_tail_and_median_absolute_deviation() {
        let corpus = CalibrationCorpus::pinned_v1();
        let evidence = aggregate_evidence(
            &candidate(Language::English),
            &context(),
            &corpus,
            Language::English,
            &perfect_observations(&corpus, Language::English),
        )
        .unwrap();
        assert_eq!(evidence.measurements.release_p50_ms, 200);
        assert_eq!(evidence.measurements.release_p95_ms, 400);
        assert_eq!(evidence.measurements.release_dispersion_ms, 100);
        assert_eq!(evidence.measurements.realtime_factor_milli, 50);
        assert_eq!(evidence.measurements.confidence_per_mille, 900);
        assert!(evidence.is_structurally_valid());
    }

    #[test]
    fn aggregation_rejects_duplicates_partial_runs_and_numeric_forgery() {
        let corpus = CalibrationCorpus::pinned_v1();
        let mut observations = perfect_observations(&corpus, Language::English);
        observations.pop();
        assert_eq!(
            aggregate_evidence(
                &candidate(Language::English),
                &context(),
                &corpus,
                Language::English,
                &observations,
            ),
            Err(AggregationError::InvalidCaseSet)
        );

        let mut observations = perfect_observations(&corpus, Language::English);
        observations[1].case_id = observations[0].case_id.clone();
        assert_eq!(
            aggregate_evidence(
                &candidate(Language::English),
                &context(),
                &corpus,
                Language::English,
                &observations,
            ),
            Err(AggregationError::InvalidCaseSet)
        );

        let mut observations = perfect_observations(&corpus, Language::English);
        observations[0].available_memory_mib = 0;
        assert_eq!(
            aggregate_evidence(
                &candidate(Language::English),
                &context(),
                &corpus,
                Language::English,
                &observations,
            ),
            Err(AggregationError::InvalidMeasurement)
        );
    }

    #[test]
    fn biased_or_cross_language_measurements_fail_closed() {
        let corpus = CalibrationCorpus::pinned_v1();
        let mut observations = perfect_observations(&corpus, Language::English);
        observations[2].thermal_condition = ThermalCondition::Elevated;
        assert_eq!(
            aggregate_evidence(
                &candidate(Language::English),
                &context(),
                &corpus,
                Language::English,
                &observations,
            ),
            Err(AggregationError::ThermalBias)
        );
        assert_eq!(
            aggregate_evidence(
                &candidate(Language::English),
                &context(),
                &corpus,
                Language::PortugueseBrazil,
                &perfect_observations(&corpus, Language::PortugueseBrazil),
            ),
            Err(AggregationError::UnsupportedLanguage)
        );
    }

    #[test]
    fn aggregation_is_order_independent_and_percentages_stay_bounded() {
        let corpus = CalibrationCorpus::pinned_v1();
        let original = perfect_observations(&corpus, Language::English);
        let expected = aggregate_evidence(
            &candidate(Language::English),
            &context(),
            &corpus,
            Language::English,
            &original,
        )
        .unwrap();
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        let order = [a, b, c, d];
                        if order.iter().copied().collect::<BTreeSet<_>>().len() != 4 {
                            continue;
                        }
                        let permuted = order.map(|index| original[index].clone());
                        let actual = aggregate_evidence(
                            &candidate(Language::English),
                            &context(),
                            &corpus,
                            Language::English,
                            &permuted,
                        )
                        .unwrap();
                        assert_eq!(actual, expected);
                        assert!(
                            actual.measurements.release_p50_ms
                                <= actual.measurements.release_p95_ms
                        );
                        assert!(actual.measurements.word_error_per_mille <= 1_000);
                        assert!(actual.measurements.character_error_per_mille <= 1_000);
                    }
                }
            }
        }
    }
}
