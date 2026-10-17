//! Curated English Vosk choices; downloads never activate a model implicitly.
use crate::setup_host::{HttpsFetcher, ManagedRoot, PinnedCatalog};
use eframe::egui;
use phorminx_setup::{ActionKey, AssetId, SetupAction};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver},
};

pub struct VoskChoice {
    pub id: &'static str,
    pub directory: &'static str,
    pub label: &'static str,
    pub bytes: u64,
    pub digest: &'static str,
    pub url: &'static str,
    pub detail: &'static str,
}

// SHA-256 and byte counts independently measured by streaming complete archives
// directly from the publisher. Models retain Apache-2.0 license. Sizes are disk
// downloads, not a promise about recognition memory or accuracy on a given voice.
pub const MODELS: &[VoskChoice] = &[
    VoskChoice {
        id: "vosk-model-small-en-us-0-15",
        directory: "vosk-model-small-en-us-0.15",
        label: "Lightweight",
        bytes: 41_205_931,
        digest: "30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498",
        url: "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip",
        detail: "40 MB download · low memory · current default",
    },
    VoskChoice {
        id: "vosk-model-en-us-0-22-lgraph",
        directory: "vosk-model-en-us-0.22-lgraph",
        label: "Expanded vocabulary",
        bytes: 130_557_655,
        digest: "d9838b4aaa82a75c4a17f5aca300eaca129aaab2a7cbf951bafbb500eb9c4334",
        url: "https://alphacephei.com/vosk/models/vosk-model-en-us-0.22-lgraph.zip",
        detail: "125 MB download · dynamic graph · compare on your own voice",
    },
    VoskChoice {
        id: "vosk-model-en-us-0-22",
        directory: "vosk-model-en-us-0.22",
        label: "High-resource English",
        bytes: 1_913_365_522,
        digest: "47f9a81ebb039dbb0bd319175c36ac393c0893b796c2b6303e64cf58c27b69f6",
        url: "https://alphacephei.com/vosk/models/vosk-model-en-us-0.22.zip",
        detail: "1.8 GB download · several GB RAM · for capable computers",
    },
    VoskChoice {
        id: "vosk-model-en-us-0-42-gigaspeech",
        directory: "vosk-model-en-us-0.42-gigaspeech",
        label: "GigaSpeech English",
        bytes: 2_423_807_363,
        digest: "d6755c99b0bc8fb3c56962690dc55e9792abd92550dda0b8899f17da55bf3f30",
        url: "https://alphacephei.com/vosk/models/vosk-model-en-us-0.42-gigaspeech.zip",
        detail: "2.3 GB download · high RAM use · podcast/general speech candidate",
    },
];

/// Only a successful background native probe can construct this activation token.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedVosk {
    runtime: PathBuf,
    model: PathBuf,
}
impl ValidatedVosk {
    pub(crate) fn apply_to(self, settings: &mut crate::settings::Settings) {
        settings.recognition.mode = crate::settings::RecognitionMode::Instant;
        settings.recognition.language = "en".into();
        settings.recognition.instant_runtime_path = self.runtime;
        settings.recognition.instant_model_path = self.model;
    }
}

fn repair_hint(path: &Path, reason: &str) -> String {
    format!(
        "{reason} No existing files or settings were changed. Close Phorminx completely, rename this exact asset folder to keep it as a backup, then reopen and choose Download / check installed: {}",
        path.display()
    )
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// A receipt proves the original acquisition, not the current contents. This is
/// a bounded filesystem/layout check only; native validation is separate.
fn check_cached_asset(path: &Path, runtime: bool) -> Result<(), String> {
    let mut pending = vec![(path.to_path_buf(), 0usize)];
    let mut entries = 0usize;
    while let Some((entry, depth)) = pending.pop() {
        entries += 1;
        let metadata = std::fs::symlink_metadata(&entry)
            .map_err(|_| repair_hint(path, "An installed asset is missing or unreadable."))?;
        if linked(&metadata) || entries > 10_000 || depth > 24 {
            return Err(repair_hint(
                path,
                "The installed asset contains an unsafe link or exceeds layout limits.",
            ));
        }
        if metadata.is_dir() {
            for child in std::fs::read_dir(&entry)
                .map_err(|_| repair_hint(path, "An installed asset directory could not be read."))?
            {
                let child = child.map_err(|_| {
                    repair_hint(path, "An installed asset directory could not be read.")
                })?;
                if pending.len() + entries >= 10_000 {
                    return Err(repair_hint(
                        path,
                        "The installed asset exceeds layout limits.",
                    ));
                }
                pending.push((child.path(), depth + 1));
            }
        } else if !metadata.is_file() {
            return Err(repair_hint(
                path,
                "The installed asset contains a non-file entry.",
            ));
        }
    }
    let nonempty = |relative: &str| {
        std::fs::metadata(path.join(relative)).is_ok_and(|m| m.is_file() && m.len() > 0)
    };
    let valid = if runtime {
        [
            "libvosk.dll",
            "libgcc_s_seh-1.dll",
            "libstdc++-6.dll",
            "libwinpthread-1.dll",
        ]
        .iter()
        .all(|file| nonempty(file))
    } else {
        nonempty("am/final.mdl")
            && nonempty("conf/model.conf")
            && (nonempty("graph/HCLG.fst")
                || (nonempty("graph/HCLr.fst") && nonempty("graph/Gr.fst")))
    };
    if valid {
        Ok(())
    } else {
        Err(repair_hint(
            path,
            "Required installed asset files are missing or empty.",
        ))
    }
}

fn check_bundle(runtime: &Path, model: &Path) -> Result<(), String> {
    check_cached_asset(runtime, true)?;
    check_cached_asset(model, false)
}

fn install(
    id: &str,
    cancel: &AtomicBool,
    progress: &AtomicU64,
) -> Result<(PathBuf, PathBuf), String> {
    if !MODELS.iter().any(|model| model.id == id) {
        return Err("Unknown curated model.".into());
    }
    let catalog = PinnedCatalog::phorminx().map_err(|e| e.to_string())?;
    let root = ManagedRoot::from_local_app_data().map_err(|e| e.to_string())?;
    // No recovery cleanup here: another setup transaction could be active.
    let mut paths = Vec::new();
    for asset in ["vosk-runtime-win64-0-3-45", id] {
        let identity = AssetId::new(asset).map_err(|e| e.to_string())?;
        let pinned = catalog
            .artifact(&identity)
            .ok_or("Curated model is unavailable.")?;
        if let Some(installed) = root
            .installed()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|item| item.receipt.asset().asset_id() == &identity)
        {
            check_cached_asset(&installed.target, asset == "vosk-runtime-win64-0-3-45")?;
            paths.push(installed.target);
        } else {
            let action = SetupAction::for_key(ActionKey::DownloadArtifact {
                artifact: Box::new(pinned.descriptor().clone()),
            })
            .map_err(|e| e.to_string())?;
            let result = root
                .install(
                    &action,
                    pinned,
                    &HttpsFetcher,
                    cancel,
                    &mut |_, received, _| {
                        progress.store(received, Ordering::Relaxed);
                    },
                )
                .map_err(|e| e.to_string())?;
            check_cached_asset(&result.target, asset == "vosk-runtime-win64-0-3-45")?;
            paths.push(result.target);
        }
    }
    Ok((paths.remove(0), paths.remove(0)))
}

#[derive(Default)]
pub struct VoskModels {
    pub open: bool,
    selected: usize,
    pending: Option<Receiver<Result<(PathBuf, PathBuf), String>>>,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU64>,
    downloaded: Option<(usize, PathBuf, PathBuf)>,
    notice: String,
    validation: Option<Receiver<Result<ValidatedVosk, String>>>,
    validated: Option<ValidatedVosk>,
    validation_lease: Option<crate::performance_runtime::RuntimeActivityLease>,
}
impl Drop for VoskModels {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl VoskModels {
    pub fn poll(&mut self) {
        if let Some(rx) = &self.validation {
            match rx.try_recv() {
                Ok(Ok(proof)) => {
                    self.validation = None;
                    self.validated = Some(proof);
                    self.notice = "Model validated. Applying when idle…".into();
                }
                Ok(Err(error)) => {
                    self.validation = None;
                    self.validation_lease = None;
                    self.notice = error;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.validation = None;
                    self.validation_lease = None;
                    self.notice =
                        "Native validation worker exited. The current model was not changed."
                            .into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(Ok((runtime, model))) => {
                    self.pending = None;
                    self.downloaded = Some((self.selected, runtime, model));
                    self.notice="Asset layout checked. New downloads were SHA-256 verified; cached contents were not rehashed. Choose Use model for native validation before switching.".into();
                }
                Ok(Err(error)) => {
                    self.pending = None;
                    self.downloaded = None;
                    self.notice = format!("Model setup did not complete: {error}");
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.notice = "Model worker stopped. Retry; settings were not changed.".into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.validation.is_some() || self.validated.is_some()
    }
    pub fn take_validated(&mut self) -> Option<ValidatedVosk> {
        let proof = self.validated.take();
        if proof.is_some() {
            self.validation_lease = None;
        }
        proof
    }
    pub fn show(&mut self, ctx: &egui::Context, idle: bool) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Instant models · Phorminx").open(&mut open).default_width(530.0).show(ctx,|ui|{
            ui.heading("Choose the capacity you need");
            ui.label("English speech recognition · all processing stays local");
            ui.add_space(12.0);
            ui.add_enabled_ui(!self.busy(),|ui|{
                for (index,model) in MODELS.iter().enumerate(){
                    if ui.radio_value(&mut self.selected,index,model.label).changed(){self.downloaded=None;}
                    ui.weak(model.detail);
                    ui.add_space(8.0);
                }
            });
            ui.label("Large models may require up to 16 GB of model memory. Both-source meetings load two recognizers. More resources do not guarantee perfect recognition.");
            ui.weak("Downloads come from alphacephei.com; the runtime comes from the official Vosk GitHub release. Files are size/hash verified before safe extraction. Allow extra disk space for extraction.");
            ui.horizontal(|ui|{
                if ui.add_enabled(idle&&!self.busy(),egui::Button::new("Download / check installed")).clicked(){
                    self.downloaded=None;
                    let model=&MODELS[self.selected];
                    let cancel=Arc::new(AtomicBool::new(false));self.cancel=cancel.clone();
                    let progress=Arc::new(AtomicU64::new(0));self.progress=progress.clone();
                    let id=model.id;
                    let(tx,rx)=mpsc::sync_channel(1);
                    match std::thread::Builder::new().name("phorminx-vosk-download".into()).spawn(move||{let _=tx.send(install(id,&cancel,&progress));}){
                        Ok(_)=>{self.pending=Some(rx);self.notice="Downloading. Your current model remains active.".into();}
                        Err(_)=>self.notice="Could not start model download.".into(),
                    }
                }
                if self.pending.is_some()&&ui.button("Cancel download").clicked(){self.cancel.store(true,Ordering::Release);}
                if let Some((_,runtime,model))=&self.downloaded
                    &&ui.add_enabled(idle&&!self.busy(),egui::Button::new("Use for English Instant")).clicked(){
                    let runtime=runtime.clone();let model=model.clone();
                    match crate::performance_runtime::production_workload_coordinator().try_begin(crate::performance_runtime::RuntimeActivityKind::CalibrationCapture) {
                        Ok(lease)=>{
                            let(tx,rx)=mpsc::sync_channel(1);
                            match std::thread::Builder::new().name("phorminx-vosk-validation".into()).spawn(move||{
                                let result=check_bundle(&runtime,&model).and_then(|()| {
                                    if phorminx_vosk::inspect(&runtime,&model,"en").is_ready(){Ok(ValidatedVosk{runtime,model})}
                                    else{Err(format!("Native validation failed; the active model is unchanged. First check available memory. If the model files are damaged: {}", repair_hint(&model,"The cached layout alone cannot establish model integrity.")))}
                                });
                                let _=tx.send(result);
                            }) {
                                Ok(_)=>{self.validation=Some(rx);self.validation_lease=Some(lease);self.notice="Loading and validating model in the background. Larger models can take time and memory. The current settings are unchanged until success.".into();}
                                Err(_)=>self.notice="Could not start native validation.".into(),
                            }
                        }
                        Err(_)=>self.notice="Finish recording or other AI work before switching recognition models.".into(),
                    }
                }
            });
            if self.pending.is_some(){ui.label(format!("{} MB received",self.progress.load(Ordering::Relaxed)/1024/1024));ctx.request_repaint_after(std::time::Duration::from_millis(200));}
            if !self.notice.is_empty(){ui.separator();ui.label(&self.notice);}
        });
        self.open = open;
        if self.validation.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cached_fixture(root: &Path) -> (PathBuf, PathBuf) {
        let runtime = root.join("runtime");
        let model = root.join("vosk-model-en-us-0.22");
        for (base, files) in [
            (
                &runtime,
                vec![
                    "libvosk.dll",
                    "libgcc_s_seh-1.dll",
                    "libstdc++-6.dll",
                    "libwinpthread-1.dll",
                ],
            ),
            (
                &model,
                vec!["am/final.mdl", "conf/model.conf", "graph/HCLG.fst"],
            ),
        ] {
            for file in files {
                let path = base.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"synthetic nonempty layout, not a native model").unwrap();
            }
        }
        (runtime, model)
    }
    #[test]
    fn cached_layout_checks_missing_empty_and_both_graph_formats_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let (runtime, model) = cached_fixture(temp.path());
        check_bundle(&runtime, &model).unwrap();
        // Presence is intentionally not represented as hash or native validation.
        let untouched = std::fs::read(model.join("am/final.mdl")).unwrap();
        std::fs::write(model.join("graph/HCLG.fst"), b"").unwrap();
        let error = check_bundle(&runtime, &model).unwrap_err();
        assert!(error.contains("Close Phorminx") && error.contains("backup"));
        assert!(error.contains(model.to_str().unwrap()));
        assert_eq!(
            std::fs::read(model.join("am/final.mdl")).unwrap(),
            untouched
        );
        std::fs::write(model.join("graph/HCLr.fst"), b"split").unwrap();
        assert!(check_bundle(&runtime, &model).is_err());
        std::fs::write(model.join("graph/Gr.fst"), b"split").unwrap();
        check_bundle(&runtime, &model).unwrap();
        std::fs::remove_file(runtime.join("libvosk.dll")).unwrap();
        assert!(
            check_bundle(&runtime, &model)
                .unwrap_err()
                .contains(runtime.to_str().unwrap())
        );
        assert!(model.join("graph/Gr.fst").exists());
    }
    #[test]
    fn failed_cached_check_clears_old_activation_and_success_is_honest_about_integrity() {
        let mut panel = VoskModels::default();
        panel.downloaded = Some((0, PathBuf::from("old-runtime"), PathBuf::from("old-model")));
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(Err("damaged cached layout".into())).unwrap();
        panel.pending = Some(rx);
        panel.poll();
        assert!(panel.downloaded.is_none());
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(Ok((
            PathBuf::from("checked-runtime"),
            PathBuf::from("checked-model"),
        )))
        .unwrap();
        panel.pending = Some(rx);
        panel.poll();
        assert!(panel.notice.contains("cached contents were not rehashed"));
        assert!(panel.validated.is_none());
    }
    #[test]
    #[cfg(windows)]
    fn cached_asset_links_are_rejected_without_following_or_changing_target() {
        let temp = tempfile::tempdir().unwrap();
        let (runtime, model) = cached_fixture(temp.path());
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("kept.txt"), b"untouched").unwrap();
        if let Err(error) = std::os::windows::fs::symlink_dir(&outside, model.join("linked")) {
            eprintln!("Symlink fixture unavailable on this Windows account: {error}");
            return;
        }
        assert!(
            check_bundle(&runtime, &model)
                .unwrap_err()
                .contains("unsafe link")
        );
        assert_eq!(
            std::fs::read(outside.join("kept.txt")).unwrap(),
            b"untouched"
        );
    }
    #[test]
    fn every_resource_tier_has_a_pinned_official_download() {
        assert_eq!(MODELS.len(), 4);
        let catalog = PinnedCatalog::phorminx().unwrap();
        for model in MODELS {
            let artifact = catalog.artifact(&AssetId::new(model.id).unwrap()).unwrap();
            assert_eq!(artifact.descriptor().size_bytes(), model.bytes);
            assert_eq!(artifact.descriptor().source_url(), model.url);
            assert!(
                model
                    .url
                    .starts_with("https://alphacephei.com/vosk/models/")
            );
            assert_eq!(model.digest.len(), 64);
        }
    }
    #[test]
    fn arbitrary_model_id_rejected_without_network() {
        assert!(
            install(
                "https://evil.example",
                &AtomicBool::new(false),
                &AtomicU64::new(0)
            )
            .is_err()
        );
    }
}
