use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    BackendKind, BenchmarkEvidence, BenchmarkProtocol, ContentFreeId, ContentionCondition,
    EngineKind, Language, ModelClass, ThermalCondition,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationPreference {
    Balanced,
    Fidelity,
    Fastest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecommendationPolicy {
    pub protocol: BenchmarkProtocol,
    pub minimum_protected_token_exact_per_mille: u16,
    pub maximum_word_error_per_mille: u16,
    pub maximum_hallucination_per_mille: u16,
    pub maximum_release_p95_ms: u64,
    pub maximum_realtime_factor_milli: u32,
    pub minimum_memory_reserve_mib: u32,
    pub minimum_run_count: u32,
    pub minimum_confidence_per_mille: u16,
    pub maximum_cold_load_ms: u64,
    pub maximum_warm_load_ms: u64,
    pub maximum_release_dispersion_ms: u64,
}

impl RecommendationPolicy {
    #[must_use]
    pub fn interactive(protocol_id: ContentFreeId) -> Self {
        Self {
            protocol: BenchmarkProtocol {
                protocol_id,
                minimum_speech_samples: 3,
                minimum_silence_samples: 1,
            },
            minimum_protected_token_exact_per_mille: 1_000,
            maximum_word_error_per_mille: 250,
            maximum_hallucination_per_mille: 0,
            maximum_release_p95_ms: 900,
            maximum_realtime_factor_milli: 250,
            minimum_memory_reserve_mib: 512,
            minimum_run_count: 4,
            minimum_confidence_per_mille: 900,
            maximum_cold_load_ms: 10_000,
            maximum_warm_load_ms: 2_000,
            maximum_release_dispersion_ms: 300,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CandidateEvidence {
    pub evidence: BenchmarkEvidence,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    InvalidEvidence,
    ProtocolMismatch,
    LanguageIncompatible,
    PortugueseInstantNotCertified,
    LoadFailed,
    InsufficientConfidence,
    WordErrorTooHigh,
    ProtectedTokensChanged,
    HallucinationDetected,
    InsufficientMemory,
    ReleaseLatencyTooHigh,
    RealtimeFactorTooHigh,
    FallbackObserved,
    LoadLatencyTooHigh,
    DispersionTooHigh,
    ThermalStateUnacceptable,
    SystemContentionObserved,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RejectedCandidate {
    pub candidate_id: ContentFreeId,
    pub reasons: BTreeSet<ExclusionReason>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum RecommendationOutcome {
    Recommended {
        candidate_id: ContentFreeId,
        engine: EngineKind,
        backend: BackendKind,
        model_class: ModelClass,
    },
    ConservativeFallback {
        language: Language,
        engine: EngineKind,
        model_class: ModelClass,
        reason: ExclusionReason,
    },
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    pub outcome: RecommendationOutcome,
    /// Sorted by candidate ID; reasons are also sorted. This makes the same
    /// evidence set byte-for-byte reproducible regardless of discovery order.
    pub rejected_candidates: Vec<RejectedCandidate>,
}

pub struct RecommendationEngine;

impl RecommendationEngine {
    #[must_use]
    pub fn recommend(
        language: Language,
        preference: RecommendationPreference,
        policy: &RecommendationPolicy,
        candidates: impl IntoIterator<Item = CandidateEvidence>,
    ) -> Recommendation {
        let mut candidates = candidates.into_iter().collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.evidence
                .candidate_id
                .cmp(&right.evidence.candidate_id)
                .then_with(|| left.evidence.cmp(&right.evidence))
        });
        // Repeated observations for one candidate do not get extra voting
        // weight. Keep the lexicographically first complete evidence record.
        candidates
            .dedup_by(|left, right| left.evidence.candidate_id == right.evidence.candidate_id);

        let mut eligible = Vec::new();
        let mut rejected = Vec::new();
        for candidate in candidates {
            let reasons = exclusion_reasons(language, policy, &candidate.evidence);
            if reasons.is_empty() {
                eligible.push(candidate);
            } else {
                rejected.push(RejectedCandidate {
                    candidate_id: candidate.evidence.candidate_id.clone(),
                    reasons,
                });
            }
        }

        eligible.sort_by(|left, right| compare_candidates(preference, left, right));
        let outcome = eligible.first().map_or_else(
            || {
                let insufficient_only = rejected.is_empty()
                    || rejected.iter().all(|candidate| {
                        candidate.reasons.iter().all(|reason| {
                            matches!(
                                reason,
                                ExclusionReason::InsufficientConfidence
                                    | ExclusionReason::ProtocolMismatch
                            )
                        })
                    });
                if insufficient_only {
                    RecommendationOutcome::ConservativeFallback {
                        language,
                        engine: EngineKind::Accurate,
                        model_class: ModelClass::Base,
                        reason: ExclusionReason::InsufficientConfidence,
                    }
                } else {
                    RecommendationOutcome::Unavailable
                }
            },
            |selected| RecommendationOutcome::Recommended {
                candidate_id: selected.evidence.candidate_id.clone(),
                engine: selected.evidence.engine,
                backend: selected.evidence.backend,
                model_class: selected.evidence.model_class,
            },
        );
        Recommendation {
            outcome,
            rejected_candidates: rejected,
        }
    }
}

fn exclusion_reasons(
    language: Language,
    policy: &RecommendationPolicy,
    evidence: &BenchmarkEvidence,
) -> BTreeSet<ExclusionReason> {
    let mut reasons = BTreeSet::new();
    if !evidence.is_structurally_valid() {
        reasons.insert(ExclusionReason::InvalidEvidence);
    }
    if evidence.protocol_id != policy.protocol.protocol_id {
        reasons.insert(ExclusionReason::ProtocolMismatch);
    }
    if !evidence.supported_languages.contains(&language) {
        reasons.insert(ExclusionReason::LanguageIncompatible);
    }
    if language == Language::PortugueseBrazil
        && evidence.engine == EngineKind::Instant
        && !evidence.pt_brazil_instant_certified
    {
        reasons.insert(ExclusionReason::PortugueseInstantNotCertified);
    }
    if !evidence.loaded {
        reasons.insert(ExclusionReason::LoadFailed);
    }
    if evidence.measurements.speech_samples < policy.protocol.minimum_speech_samples
        || evidence.measurements.silence_samples < policy.protocol.minimum_silence_samples
        || evidence.measurements.run_count < policy.minimum_run_count
        || evidence.measurements.confidence_per_mille < policy.minimum_confidence_per_mille
    {
        reasons.insert(ExclusionReason::InsufficientConfidence);
    }
    if evidence.measurements.word_error_per_mille > policy.maximum_word_error_per_mille {
        reasons.insert(ExclusionReason::WordErrorTooHigh);
    }
    if evidence.measurements.protected_token_exact_per_mille
        < policy.minimum_protected_token_exact_per_mille
    {
        reasons.insert(ExclusionReason::ProtectedTokensChanged);
    }
    if evidence.measurements.hallucination_per_mille > policy.maximum_hallucination_per_mille {
        reasons.insert(ExclusionReason::HallucinationDetected);
    }
    if evidence
        .measurements
        .peak_working_set_mib
        .saturating_add(policy.minimum_memory_reserve_mib)
        > evidence.measurements.available_memory_mib
    {
        reasons.insert(ExclusionReason::InsufficientMemory);
    }
    if evidence.measurements.release_p95_ms > policy.maximum_release_p95_ms {
        reasons.insert(ExclusionReason::ReleaseLatencyTooHigh);
    }
    if evidence.measurements.realtime_factor_milli > policy.maximum_realtime_factor_milli {
        reasons.insert(ExclusionReason::RealtimeFactorTooHigh);
    }
    if evidence.measurements.fallback_count > 0 {
        reasons.insert(ExclusionReason::FallbackObserved);
    }
    if evidence.measurements.cold_load_ms > policy.maximum_cold_load_ms
        || evidence.measurements.warm_load_ms > policy.maximum_warm_load_ms
    {
        reasons.insert(ExclusionReason::LoadLatencyTooHigh);
    }
    if evidence.measurements.release_dispersion_ms > policy.maximum_release_dispersion_ms {
        reasons.insert(ExclusionReason::DispersionTooHigh);
    }
    if evidence.measurements.thermal_condition != ThermalCondition::Nominal {
        reasons.insert(ExclusionReason::ThermalStateUnacceptable);
    }
    if evidence.measurements.contention_condition != ContentionCondition::Idle {
        reasons.insert(ExclusionReason::SystemContentionObserved);
    }
    reasons
}

fn compare_candidates(
    preference: RecommendationPreference,
    left: &CandidateEvidence,
    right: &CandidateEvidence,
) -> Ordering {
    let left = &left.evidence;
    let right = &right.evidence;
    match preference {
        RecommendationPreference::Fastest => (
            left.measurements.release_p95_ms,
            left.measurements.word_error_per_mille,
            model_rank(left.model_class),
            &left.candidate_id,
        )
            .cmp(&(
                right.measurements.release_p95_ms,
                right.measurements.word_error_per_mille,
                model_rank(right.model_class),
                &right.candidate_id,
            )),
        RecommendationPreference::Balanced | RecommendationPreference::Fidelity => (
            left.measurements.word_error_per_mille,
            left.measurements.character_error_per_mille,
            model_rank(left.model_class),
            left.measurements.release_p95_ms,
            &left.candidate_id,
        )
            .cmp(&(
                right.measurements.word_error_per_mille,
                right.measurements.character_error_per_mille,
                model_rank(right.model_class),
                right.measurements.release_p95_ms,
                &right.candidate_id,
            )),
    }
}

const fn model_rank(model: ModelClass) -> u8 {
    match model {
        ModelClass::Base => 0,
        ModelClass::Tiny => 1,
        ModelClass::Other => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BenchmarkSampleSummary, Sha256Digest};

    fn policy() -> RecommendationPolicy {
        RecommendationPolicy::interactive(ContentFreeId::new("setup-v1").unwrap())
    }

    fn candidate(
        id: &str,
        engine: EngineKind,
        class: ModelClass,
        language: Language,
        word_error: u16,
        release_ms: u64,
    ) -> CandidateEvidence {
        CandidateEvidence {
            evidence: BenchmarkEvidence {
                schema_version: BenchmarkEvidence::SCHEMA_VERSION,
                protocol_id: ContentFreeId::new("setup-v1").unwrap(),
                build_id: ContentFreeId::new("build-1").unwrap(),
                candidate_id: ContentFreeId::new(id).unwrap(),
                device_id: ContentFreeId::new("device-class-1").unwrap(),
                driver_id: ContentFreeId::new("driver-1").unwrap(),
                engine,
                backend: if engine == EngineKind::Instant {
                    BackendKind::VoskNative
                } else {
                    BackendKind::Vulkan
                },
                model_class: class,
                model_digest: Sha256Digest::new("a".repeat(64)).unwrap(),
                supported_languages: [language].into_iter().collect(),
                pt_brazil_instant_certified: false,
                loaded: true,
                measurements: BenchmarkSampleSummary {
                    speech_samples: 3,
                    silence_samples: 1,
                    run_count: 4,
                    cold_load_ms: 400,
                    warm_load_ms: 100,
                    release_p50_ms: release_ms / 2,
                    release_p95_ms: release_ms,
                    release_dispersion_ms: 50,
                    confidence_per_mille: 950,
                    realtime_factor_milli: 100,
                    word_error_per_mille: word_error,
                    character_error_per_mille: word_error,
                    protected_token_exact_per_mille: 1_000,
                    hallucination_per_mille: 0,
                    peak_working_set_mib: 500,
                    available_memory_mib: 4_000,
                    fallback_count: 0,
                    thermal_condition: ThermalCondition::Nominal,
                    contention_condition: ContentionCondition::Idle,
                },
            },
        }
    }

    #[test]
    fn recommendation_is_reproducible_across_enumeration_orders() {
        let base = candidate(
            "base",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::English,
            50,
            700,
        );
        let tiny = candidate(
            "tiny",
            EngineKind::Accurate,
            ModelClass::Tiny,
            Language::English,
            100,
            300,
        );
        let first = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Balanced,
            &policy(),
            [tiny.clone(), base.clone()],
        );
        let second = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Balanced,
            &policy(),
            [base, tiny],
        );
        assert_eq!(first, second);
    }

    #[test]
    fn balanced_prefers_base_quality_over_tiny_speed() {
        let recommendation = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Balanced,
            &policy(),
            [
                candidate(
                    "tiny-fast",
                    EngineKind::Accurate,
                    ModelClass::Tiny,
                    Language::English,
                    120,
                    250,
                ),
                candidate(
                    "base-faithful",
                    EngineKind::Accurate,
                    ModelClass::Base,
                    Language::English,
                    40,
                    800,
                ),
            ],
        );
        assert!(matches!(
            recommendation.outcome,
            RecommendationOutcome::Recommended { candidate_id, .. }
                if candidate_id.as_str() == "base-faithful"
        ));
    }

    #[test]
    fn fastest_still_enforces_quality_gates() {
        let mut unsafe_tiny = candidate(
            "tiny-unsafe",
            EngineKind::Accurate,
            ModelClass::Tiny,
            Language::English,
            10,
            100,
        );
        unsafe_tiny
            .evidence
            .measurements
            .protected_token_exact_per_mille = 999;
        let safe = candidate(
            "base-safe",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::English,
            100,
            800,
        );
        let recommendation = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Fastest,
            &policy(),
            [unsafe_tiny, safe],
        );
        assert!(matches!(
            recommendation.outcome,
            RecommendationOutcome::Recommended { candidate_id, .. }
                if candidate_id.as_str() == "base-safe"
        ));
        assert!(
            recommendation.rejected_candidates[0]
                .reasons
                .contains(&ExclusionReason::ProtectedTokensChanged)
        );
    }

    #[test]
    fn portuguese_instant_requires_explicit_corpus_certification() {
        let instant = candidate(
            "instant-pt",
            EngineKind::Instant,
            ModelClass::Other,
            Language::PortugueseBrazil,
            40,
            100,
        );
        let accurate = candidate(
            "base-multilingual",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::PortugueseBrazil,
            80,
            800,
        );
        let recommendation = RecommendationEngine::recommend(
            Language::PortugueseBrazil,
            RecommendationPreference::Fastest,
            &policy(),
            [instant, accurate],
        );
        assert!(matches!(
            recommendation.outcome,
            RecommendationOutcome::Recommended { candidate_id, .. }
                if candidate_id.as_str() == "base-multilingual"
        ));
        assert!(
            recommendation.rejected_candidates[0]
                .reasons
                .contains(&ExclusionReason::PortugueseInstantNotCertified)
        );
    }

    #[test]
    fn insufficient_confidence_uses_conservative_language_aware_fallback() {
        let mut insufficient = candidate(
            "base-multilingual",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::PortugueseBrazil,
            0,
            100,
        );
        insufficient.evidence.measurements.speech_samples = 1;
        let recommendation = RecommendationEngine::recommend(
            Language::PortugueseBrazil,
            RecommendationPreference::Balanced,
            &policy(),
            [insufficient],
        );
        assert_eq!(
            recommendation.outcome,
            RecommendationOutcome::ConservativeFallback {
                language: Language::PortugueseBrazil,
                engine: EngineKind::Accurate,
                model_class: ModelClass::Base,
                reason: ExclusionReason::InsufficientConfidence,
            }
        );
    }

    #[test]
    fn hard_resource_failure_never_becomes_a_fallback_recommendation() {
        let mut unsafe_candidate = candidate(
            "base-memory-unsafe",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::English,
            0,
            100,
        );
        unsafe_candidate.evidence.measurements.available_memory_mib = 900;
        unsafe_candidate.evidence.measurements.peak_working_set_mib = 500;
        let recommendation = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Balanced,
            &policy(),
            [unsafe_candidate],
        );
        assert_eq!(recommendation.outcome, RecommendationOutcome::Unavailable);
    }

    #[test]
    fn fallback_or_invalid_engine_backend_evidence_is_never_recommended() {
        let mut fallback = candidate(
            "fallback",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::English,
            0,
            100,
        );
        fallback.evidence.measurements.fallback_count = 1;
        let mut impossible = candidate(
            "impossible",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::English,
            0,
            100,
        );
        impossible.evidence.backend = BackendKind::VoskNative;
        let recommendation = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Balanced,
            &policy(),
            [fallback, impossible],
        );
        assert_eq!(recommendation.outcome, RecommendationOutcome::Unavailable);
        assert!(recommendation.rejected_candidates.iter().any(|candidate| {
            candidate
                .reasons
                .contains(&ExclusionReason::FallbackObserved)
        }));
        assert!(recommendation.rejected_candidates.iter().any(|candidate| {
            candidate
                .reasons
                .contains(&ExclusionReason::InvalidEvidence)
        }));
    }

    #[test]
    fn noisy_or_thermally_biased_measurements_are_hard_excluded() {
        let mut candidate = candidate(
            "biased",
            EngineKind::Accurate,
            ModelClass::Base,
            Language::English,
            0,
            100,
        );
        candidate.evidence.measurements.release_p50_ms = 101;
        candidate.evidence.measurements.release_p95_ms = 100;
        candidate.evidence.measurements.thermal_condition = ThermalCondition::Elevated;
        candidate.evidence.measurements.contention_condition = ContentionCondition::Contended;
        let recommendation = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Balanced,
            &policy(),
            [candidate],
        );
        assert_eq!(recommendation.outcome, RecommendationOutcome::Unavailable);
        let reasons = &recommendation.rejected_candidates[0].reasons;
        assert!(reasons.contains(&ExclusionReason::InvalidEvidence));
        assert!(reasons.contains(&ExclusionReason::ThermalStateUnacceptable));
        assert!(reasons.contains(&ExclusionReason::SystemContentionObserved));
    }
}
