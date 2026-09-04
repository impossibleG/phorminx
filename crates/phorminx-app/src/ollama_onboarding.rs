//! UI-independent production composition for trusted Ollama onboarding.

use phorminx_ollama::{
    AuthorizedModelPull, CancellationToken, CuratedModelCatalog, CuratedModelId, DaemonState,
    ModelPullOutcome, ModelPullProgress, ModelPullReview, OllamaOnboarding,
    OllamaOnboardingTransport, PullFailure, UreqOnboardingTransport,
};
use phorminx_windows::{OllamaInstallation, inspect_ollama_installation};

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
    pub const fn new(transport: T, install_probe: P) -> Self {
        Self {
            onboarding: OllamaOnboarding::new(transport),
            install_probe,
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

    pub fn pull(
        &self,
        authorization: AuthorizedModelPull,
        cancel: &CancellationToken,
        progress: impl FnMut(ModelPullProgress),
    ) -> Result<ModelPullOutcome, PullFailure> {
        self.onboarding.pull(authorization, cancel, progress)
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
}
