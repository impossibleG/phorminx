//! UI-independent production composition for trusted Ollama onboarding.

use phorminx_ollama::{
    AuthorizedModelPull, CancellationToken, CuratedModelCatalog, CuratedModelId, CuratedModelState,
    DaemonState, InstalledModelIdentity, ModelPullOutcome, ModelPullProgress, ModelPullReview,
    OllamaOnboarding, OllamaOnboardingTransport, PullFailure, PullFailureKind, PullResidue,
    UreqOnboardingTransport,
};
use phorminx_windows::{
    OllamaInstallation, inspect_ollama_installation, ollama_model_disk_free_bytes,
};

use crate::performance_runtime::{
    RuntimeActivityKind, WorkloadCoordinator, production_workload_coordinator,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallPresence {
    Missing,
    Present,
    Unsafe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("the Ollama installation state could not be inspected")]
pub struct InstallProbeError;

pub trait OllamaInstallProbe: Send + Sync {
    fn inspect(&self) -> Result<InstallPresence, InstallProbeError>;

    fn available_model_disk_bytes(&self) -> Result<u64, InstallProbeError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsOllamaInstallProbe;

impl OllamaInstallProbe for WindowsOllamaInstallProbe {
    fn inspect(&self) -> Result<InstallPresence, InstallProbeError> {
        match inspect_ollama_installation().map_err(|_| InstallProbeError)? {
            OllamaInstallation::Missing => Ok(InstallPresence::Missing),
            OllamaInstallation::Present => Ok(InstallPresence::Present),
            OllamaInstallation::Unsafe => Ok(InstallPresence::Unsafe),
        }
    }

    fn available_model_disk_bytes(&self) -> Result<u64, InstallProbeError> {
        ollama_model_disk_free_bytes().map_err(|_| InstallProbeError)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OllamaHostState {
    InstallationInspectionFailed,
    UnsafeInstallation,
    Daemon(DaemonState),
}

pub struct OllamaOnboardingHost<T, P> {
    onboarding: OllamaOnboarding<T>,
    install_probe: P,
    workloads: std::sync::Arc<WorkloadCoordinator>,
}

impl OllamaOnboardingHost<UreqOnboardingTransport, WindowsOllamaInstallProbe> {
    #[must_use]
    pub fn production() -> Self {
        Self::new(
            UreqOnboardingTransport::loopback(),
            WindowsOllamaInstallProbe,
        )
    }
}

impl<T: OllamaOnboardingTransport, P: OllamaInstallProbe> OllamaOnboardingHost<T, P> {
    #[must_use]
    pub fn new(transport: T, install_probe: P) -> Self {
        Self::with_workloads(transport, install_probe, production_workload_coordinator())
    }

    fn with_workloads(
        transport: T,
        install_probe: P,
        workloads: std::sync::Arc<WorkloadCoordinator>,
    ) -> Self {
        Self {
            onboarding: OllamaOnboarding::new(transport),
            install_probe,
            workloads,
        }
    }

    #[must_use]
    pub const fn catalog(&self) -> CuratedModelCatalog {
        self.onboarding.catalog()
    }

    pub fn inspect(&self, cancel: &CancellationToken) -> OllamaHostState {
        match self.install_probe.inspect() {
            Err(_) => OllamaHostState::InstallationInspectionFailed,
            Ok(InstallPresence::Unsafe) => OllamaHostState::UnsafeInstallation,
            Ok(InstallPresence::Missing) => {
                OllamaHostState::Daemon(self.onboarding.probe(false, cancel))
            }
            Ok(InstallPresence::Present) => {
                OllamaHostState::Daemon(self.onboarding.probe(true, cancel))
            }
        }
    }

    #[must_use]
    pub fn review_pull(&self, id: CuratedModelId) -> ModelPullReview {
        self.onboarding.review_pull(id)
    }

    #[must_use]
    pub fn model_state(
        &self,
        id: CuratedModelId,
        installed: &[InstalledModelIdentity],
    ) -> CuratedModelState {
        self.onboarding.model_state(id, installed)
    }

    pub fn pull(
        &self,
        authorization: AuthorizedModelPull,
        cancel: &CancellationToken,
        progress: impl FnMut(ModelPullProgress),
    ) -> Result<ModelPullOutcome, PullFailure> {
        if cancel.is_cancelled() {
            return Err(PullFailure {
                kind: PullFailureKind::Cancelled,
                residue: PullResidue::None,
            });
        }
        if self.install_probe.inspect() != Ok(InstallPresence::Present) {
            return Err(PullFailure {
                kind: PullFailureKind::InstallationUntrusted,
                residue: PullResidue::None,
            });
        }
        let available = self
            .install_probe
            .available_model_disk_bytes()
            .map_err(|_| PullFailure {
                kind: PullFailureKind::CapacityUnavailable,
                residue: PullResidue::None,
            })?;
        let _activity = self
            .workloads
            .try_begin(RuntimeActivityKind::Ollama)
            .map_err(|_| PullFailure {
                kind: PullFailureKind::Busy,
                residue: PullResidue::None,
            })?;
        self.onboarding
            .pull(authorization, available, cancel, progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NeverTransport;

    impl OllamaOnboardingTransport for NeverTransport {
        fn version(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<phorminx_ollama::OllamaVersion, phorminx_ollama::TransportError> {
            panic!("transport must not be contacted")
        }

        fn installed_models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<phorminx_ollama::InstalledModelIdentity>, phorminx_ollama::TransportError>
        {
            panic!("transport must not be contacted")
        }

        fn pull_model(
            &self,
            _model: phorminx_ollama::CuratedModel,
            _cancel: &CancellationToken,
            _progress: &mut dyn FnMut(ModelPullProgress),
        ) -> Result<(), phorminx_ollama::TransportError> {
            panic!("transport must not be contacted")
        }
    }

    struct FixedProbe(Result<InstallPresence, InstallProbeError>);

    impl OllamaInstallProbe for FixedProbe {
        fn inspect(&self) -> Result<InstallPresence, InstallProbeError> {
            self.0
        }

        fn available_model_disk_bytes(&self) -> Result<u64, InstallProbeError> {
            Ok(u64::MAX)
        }
    }

    #[test]
    fn missing_and_unsafe_installations_never_contact_loopback() {
        let missing =
            OllamaOnboardingHost::new(NeverTransport, FixedProbe(Ok(InstallPresence::Missing)));
        assert_eq!(
            missing.inspect(&CancellationToken::new()),
            OllamaHostState::Daemon(DaemonState::NotInstalled)
        );

        let unsafe_install =
            OllamaOnboardingHost::new(NeverTransport, FixedProbe(Ok(InstallPresence::Unsafe)));
        assert_eq!(
            unsafe_install.inspect(&CancellationToken::new()),
            OllamaHostState::UnsafeInstallation
        );
    }

    #[test]
    fn inspection_errors_are_content_free() {
        let host = OllamaOnboardingHost::new(NeverTransport, FixedProbe(Err(InstallProbeError)));
        assert_eq!(
            host.inspect(&CancellationToken::new()),
            OllamaHostState::InstallationInspectionFailed
        );
        assert_eq!(
            InstallProbeError.to_string(),
            "the Ollama installation state could not be inspected"
        );
    }

    #[test]
    fn pull_rechecks_installation_and_never_contacts_an_untrusted_daemon() {
        let host = OllamaOnboardingHost::with_workloads(
            NeverTransport,
            FixedProbe(Ok(InstallPresence::Unsafe)),
            std::sync::Arc::new(WorkloadCoordinator::default()),
        );
        let review = host.review_pull(CuratedModelId::Gemma3OneB);
        let confirmation = review.confirmation().to_owned();
        let action = review.authorize(&confirmation).unwrap();
        let error = host
            .pull(action, &CancellationToken::new(), |_| {})
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::InstallationUntrusted);
        assert_eq!(error.residue, PullResidue::None);
    }
}
