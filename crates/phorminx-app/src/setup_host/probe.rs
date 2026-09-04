use phorminx_setup::{
    CapabilityId, CapabilityRecord, CapabilityValue, ContentFreeId, DegradedReason, EngineKind,
    Language, ReadyAuthority, Remedy, Sha256Digest, UnavailableReason, Usability,
};

use super::PinnedCatalog;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecognitionProbeState {
    Missing,
    InstalledUnvalidated,
    Ready {
        resident: bool,
    },
    Degraded {
        resident: bool,
        reason: DegradedReason,
    },
    Unavailable(UnavailableReason),
}

/// A normalized, process-local observation emitted by trusted probe code.
/// Remedies are deliberately absent; they are derived from the compiled
/// catalog when this value is converted into a domain record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NormalizedProbeFact {
    Microphone {
        selected_is_available: bool,
        permission_denied: bool,
        /// A concrete enumerated input exists, so changing the selection can
        /// actually repair this state. Absence must not masquerade as denial.
        selection_possible: bool,
    },
    Recognition {
        engine: EngineKind,
        language: Language,
        model_digest: Option<Sha256Digest>,
        state: RecognitionProbeState,
    },
    OllamaDaemon {
        version: Option<ContentFreeId>,
        reachable: bool,
        installed: bool,
    },
    OllamaModel {
        expected_digest: Sha256Digest,
        present_digest: Option<Sha256Digest>,
    },
    LaunchAtLogin {
        enabled: bool,
        exact_command: bool,
    },
}

impl NormalizedProbeFact {
    pub fn into_record(self, catalog: &PinnedCatalog) -> Result<CapabilityRecord, ProbeFactError> {
        match self {
            Self::Microphone {
                selected_is_available,
                permission_denied,
                selection_possible,
            } => {
                let usability = if selected_is_available {
                    Usability::Ready {
                        authority: ReadyAuthority::OperatingSystemReadback,
                    }
                } else {
                    Usability::Unavailable {
                        reason: if permission_denied {
                            UnavailableReason::PermissionDenied
                        } else {
                            UnavailableReason::Missing
                        },
                    }
                };
                let record = CapabilityRecord::new(
                    CapabilityId::Microphone,
                    Some(CapabilityValue::Microphone {
                        selected_is_available,
                    }),
                    usability,
                )?;
                if selected_is_available {
                    Ok(record)
                } else if permission_denied {
                    Ok(record.with_remedies([Remedy::GrantMicrophoneAccess])?)
                } else if selection_possible {
                    Ok(record.with_remedies([Remedy::SelectMicrophone])?)
                } else {
                    Ok(record)
                }
            }
            Self::Recognition {
                engine,
                language,
                model_digest,
                state,
            } => recognition_record(catalog, engine, language, model_digest, state),
            Self::OllamaDaemon {
                version,
                reachable,
                installed,
            } => {
                let record = CapabilityRecord::new(
                    CapabilityId::OllamaDaemon,
                    Some(CapabilityValue::ExternalTool {
                        version: version.clone(),
                    }),
                    if reachable && version.is_some() {
                        Usability::Ready {
                            authority: ReadyAuthority::LoopbackHealth,
                        }
                    } else {
                        Usability::Unavailable {
                            reason: if installed {
                                UnavailableReason::ExternalToolStopped
                            } else {
                                UnavailableReason::Missing
                            },
                        }
                    },
                )?;
                if reachable && version.is_some() {
                    Ok(record)
                } else {
                    let tool = ContentFreeId::new("ollama")
                        .map_err(|_| ProbeFactError::InternalIdentity)?;
                    Ok(record.with_remedies([if installed {
                        Remedy::StartExternalTool { tool }
                    } else {
                        Remedy::GuidedExternalInstall { tool }
                    }])?)
                }
            }
            Self::OllamaModel {
                expected_digest,
                present_digest,
            } => {
                let matches = present_digest.as_ref() == Some(&expected_digest);
                let was_present = present_digest.is_some();
                let record = CapabilityRecord::new(
                    CapabilityId::OllamaModel {
                        digest: expected_digest.clone(),
                    },
                    matches.then(|| CapabilityValue::Model {
                        digest: expected_digest.clone(),
                    }),
                    if matches {
                        Usability::Ready {
                            authority: ReadyAuthority::LoopbackHealth,
                        }
                    } else {
                        Usability::Unavailable {
                            reason: if was_present {
                                UnavailableReason::DifferentConfiguration
                            } else {
                                UnavailableReason::Missing
                            },
                        }
                    },
                )?;
                if matches {
                    Ok(record)
                } else {
                    Ok(record.with_remedies([Remedy::PullOllamaModel {
                        digest: expected_digest,
                    }])?)
                }
            }
            Self::LaunchAtLogin {
                enabled,
                exact_command,
            } => {
                let record = CapabilityRecord::new(
                    CapabilityId::LaunchAtLogin,
                    Some(CapabilityValue::LaunchAtLogin {
                        enabled,
                        exact_command,
                    }),
                    if exact_command {
                        Usability::Ready {
                            authority: ReadyAuthority::OperatingSystemReadback,
                        }
                    } else {
                        Usability::Unavailable {
                            reason: UnavailableReason::DifferentConfiguration,
                        }
                    },
                )?;
                if exact_command {
                    Ok(record)
                } else {
                    Ok(record.with_remedies([Remedy::ApplyLaunchAtLogin { enabled }])?)
                }
            }
        }
    }
}

fn recognition_record(
    catalog: &PinnedCatalog,
    engine: EngineKind,
    language: Language,
    model_digest: Option<Sha256Digest>,
    state: RecognitionProbeState,
) -> Result<CapabilityRecord, ProbeFactError> {
    let id = match engine {
        EngineKind::Accurate => CapabilityId::AccurateRecognition { language },
        EngineKind::Instant => CapabilityId::InstantRecognition { language },
    };
    let value = model_digest.map(|digest| CapabilityValue::Recognition {
        engine,
        language,
        model_digest: Some(digest),
    });
    let needs_acquisition = matches!(
        &state,
        RecognitionProbeState::Missing | RecognitionProbeState::Unavailable(_)
    );
    let needs_validation = matches!(&state, RecognitionProbeState::InstalledUnvalidated);
    let usability = match state {
        RecognitionProbeState::Missing => Usability::Unavailable {
            reason: UnavailableReason::Missing,
        },
        RecognitionProbeState::InstalledUnvalidated => Usability::InstalledUnvalidated,
        RecognitionProbeState::Ready { resident } => Usability::Ready {
            authority: if resident {
                ReadyAuthority::ResidentWorker
            } else {
                ReadyAuthority::FullProbe
            },
        },
        RecognitionProbeState::Degraded { resident, reason } => Usability::Degraded {
            authority: if resident {
                ReadyAuthority::ResidentWorker
            } else {
                ReadyAuthority::FullProbe
            },
            reason,
        },
        RecognitionProbeState::Unavailable(reason) => Usability::Unavailable { reason },
    };
    let record = CapabilityRecord::new(id, value, usability)?;
    if needs_validation {
        return Ok(record.with_remedies([Remedy::ValidateInstalled])?);
    }
    if !needs_acquisition {
        return Ok(record);
    }
    let artifacts = catalog
        .preferred_recognition_artifacts(engine, language)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if artifacts.is_empty() {
        // No pinned artifact means no executable remedy. The UI can offer the
        // accurate alternative, but the planner must not fabricate one.
        return Ok(record);
    }
    Ok(record.with_remedies([Remedy::AcquireManagedAssets { artifacts }])?)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProbeFactError {
    #[error("the probe fact could not be converted to a valid capability record")]
    InvalidRecord(#[from] phorminx_setup::CapabilityRecordError),
    #[error("an internal setup identity is invalid")]
    InternalIdentity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_microphone_does_not_invent_a_selection_or_permission_repair() {
        let catalog = PinnedCatalog::phorminx().unwrap();
        let record = NormalizedProbeFact::Microphone {
            selected_is_available: false,
            permission_denied: false,
            selection_possible: false,
        }
        .into_record(&catalog)
        .unwrap();
        assert!(record.remedies().is_empty());
    }

    #[test]
    fn permission_denial_and_missing_selection_remain_distinct() {
        let catalog = PinnedCatalog::phorminx().unwrap();
        let denied = NormalizedProbeFact::Microphone {
            selected_is_available: false,
            permission_denied: true,
            selection_possible: true,
        }
        .into_record(&catalog)
        .unwrap();
        assert_eq!(denied.remedies().len(), 1);
        assert!(denied.remedies().contains(&Remedy::GrantMicrophoneAccess));

        let selectable = NormalizedProbeFact::Microphone {
            selected_is_available: false,
            permission_denied: false,
            selection_possible: true,
        }
        .into_record(&catalog)
        .unwrap();
        assert_eq!(selectable.remedies().len(), 1);
        assert!(selectable.remedies().contains(&Remedy::SelectMicrophone));
    }

    #[test]
    fn missing_english_instant_derives_only_compiled_catalog_remedies() {
        let catalog = PinnedCatalog::phorminx().unwrap();
        let record = NormalizedProbeFact::Recognition {
            engine: EngineKind::Instant,
            language: Language::English,
            model_digest: None,
            state: RecognitionProbeState::Missing,
        }
        .into_record(&catalog)
        .unwrap();
        let Remedy::AcquireManagedAssets { artifacts } = record.remedies().first().unwrap() else {
            panic!("expected managed acquisition")
        };
        assert_eq!(artifacts.len(), 2);
        assert!(
            artifacts
                .iter()
                .all(|artifact| artifact.supports(EngineKind::Instant, Language::English))
        );
    }

    #[test]
    fn portuguese_instant_does_not_invent_an_english_download() {
        let catalog = PinnedCatalog::phorminx().unwrap();
        let record = NormalizedProbeFact::Recognition {
            engine: EngineKind::Instant,
            language: Language::PortugueseBrazil,
            model_digest: None,
            state: RecognitionProbeState::Missing,
        }
        .into_record(&catalog)
        .unwrap();
        assert!(record.remedies().is_empty());
    }

    #[test]
    fn ollama_model_identity_mismatch_never_reports_ready() {
        let catalog = PinnedCatalog::phorminx().unwrap();
        let expected = Sha256Digest::new("a".repeat(64)).unwrap();
        let present = Sha256Digest::new("b".repeat(64)).unwrap();
        let record = NormalizedProbeFact::OllamaModel {
            expected_digest: expected.clone(),
            present_digest: Some(present),
        }
        .into_record(&catalog)
        .unwrap();
        assert!(!record.usability().is_ready());
        assert!(
            record
                .remedies()
                .contains(&Remedy::PullOllamaModel { digest: expected })
        );
    }
}
