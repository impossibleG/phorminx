//! Background update checks and explicit install-on-exit consent.
use crate::updates::{self, Release};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Preferences {
    automatic: bool,
    highest_sequence: u64,
}
enum ResultEvent {
    Checked(Result<updates::VerifiedCheck, String>),
    Staged(Result<PathBuf, String>),
}

pub struct UpdateCenter {
    pub open: bool,
    directory: PathBuf,
    preferences: Preferences,
    next_check: Instant,
    pending: Option<Receiver<ResultEvent>>,
    release: Option<Release>,
    staged: Option<PathBuf>,
    notice: String,
    scheduled: bool,
    retry_after: Instant,
}

impl UpdateCenter {
    pub fn new(directory: PathBuf) -> Self {
        let preferences = std::fs::read(directory.join("preferences.json"))
            .ok()
            .filter(|bytes| bytes.len() <= 4096)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        // Spread checks between installations; no common midnight dependency.
        let jitter = std::process::id() as u64 % 300;
        let notice=std::fs::read(directory.join("packages/last-install.json")).ok().filter(|bytes|bytes.len()<=4096)
            .and_then(|bytes|serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value|value.get("status").and_then(|s|s.as_str()).map(str::to_owned))
            .map(|status|match status.as_str(){"installed"=>"The last update installer completed.","expired"=>"The last update expired before installation. Check for a fresh signed release.",_=>"The last update did not complete. The installed app was not intentionally removed; check for updates to retry."}.to_owned()).unwrap_or_default();
        Self {
            open: false,
            directory,
            preferences,
            next_check: Instant::now() + Duration::from_secs(30 + jitter),
            pending: None,
            release: None,
            staged: None,
            notice,
            scheduled: false,
            retry_after: Instant::now(),
        }
    }
    fn persist(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory)
            .map_err(|_| "Could not save update preferences.")?;
        let temporary = self
            .directory
            .join(format!("preferences-{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec(&self.preferences)
            .map_err(|_| "Could not save update preferences.")?;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| "Could not save update preferences.")?;
        use std::io::Write;
        let result = (|| {
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "Could not save update preferences.")?;
            drop(file);
            phorminx_windows::atomic_replace_file(
                &temporary,
                &self.directory.join("preferences.json"),
            )
            .map_err(|_| "Could not save update preferences.")
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result.map_err(str::to_owned)
    }
    fn check(&mut self) {
        if self.pending.is_some() || self.scheduled {
            return;
        }
        if let Err(error) = updates::configured_key() {
            self.notice = error.to_string();
            return;
        }
        let sequence = self.preferences.highest_sequence;
        let (tx, rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("phorminx-update-check".into())
            .spawn(move || {
                let _ = tx.send(ResultEvent::Checked(
                    updates::check(sequence).map_err(|e| e.to_string()),
                ));
            }) {
            Ok(_) => {
                self.pending = Some(rx);
                self.notice = "Checking signed releases…".into();
            }
            Err(_) => self.notice = "Could not start update check.".into(),
        }
        self.next_check =
            Instant::now() + Duration::from_secs(6 * 60 * 60 + (std::process::id() as u64 % 300));
    }
    fn download(&mut self) {
        if self.pending.is_some() || self.scheduled {
            return;
        }
        let Some(release) = self.release.clone() else {
            return;
        };
        let directory = self.directory.join("packages");
        let (tx, rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("phorminx-update-download".into())
            .spawn(move || {
                let _ = tx.send(ResultEvent::Staged(
                    updates::stage(&release, &directory).map_err(|e| e.to_string()),
                ));
            }) {
            Ok(_) => {
                self.pending = Some(rx);
                self.notice = "Downloading and verifying update…".into();
            }
            Err(_) => self.notice = "Could not start update download.".into(),
        }
    }
    pub fn poll(&mut self, idle: bool) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(event) => {
                    self.pending = None;
                    match event {
                        ResultEvent::Checked(Ok(check)) => {
                            if self.release != check.release {
                                self.staged = None;
                            }
                            self.preferences.highest_sequence = self
                                .preferences
                                .highest_sequence
                                .max(check.highest_sequence);
                            if let Err(error) = self.persist() {
                                self.release = None;
                                self.staged = None;
                                self.retry_after = Instant::now() + Duration::from_secs(15 * 60);
                                self.notice = error;
                                return;
                            }
                            self.notice = check.release.as_ref().map_or_else(
                                || "No newer signed stable release is available.".into(),
                                |release| format!("Version {} is available.", release.version),
                            );
                            self.release = check.release;
                        }
                        ResultEvent::Checked(Err(error)) | ResultEvent::Staged(Err(error)) => {
                            self.notice = error;
                            self.retry_after = Instant::now() + Duration::from_secs(15 * 60);
                        }
                        ResultEvent::Staged(Ok(path)) => {
                            self.staged = Some(path);
                            self.notice =
                                "Verified update ready. Installation waits until Phorminx exits."
                                    .into();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.notice = "Update worker stopped; the installed app is unchanged.".into();
                    self.retry_after = Instant::now() + Duration::from_secs(15 * 60);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.preferences.automatic
            && idle
            && self.pending.is_none()
            && !self.scheduled
            && Instant::now() >= self.retry_after
        {
            if self.staged.is_some() {
                self.schedule();
            } else if self.release.is_some() {
                self.download();
            } else if Instant::now() >= self.next_check {
                self.check();
            }
        }
    }
    fn schedule(&mut self) {
        let (Some(path), Some(release)) = (&self.staged, &self.release) else {
            return;
        };
        match updates::install_after_exit(path, release) {
            Ok(()) => {
                self.scheduled = true;
                self.notice =
                    "Update scheduled for after Phorminx exits. Recording will not be interrupted."
                        .into();
            }
            Err(error) => {
                self.notice = error.to_string();
                self.retry_after = Instant::now() + Duration::from_secs(15 * 60);
            }
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, idle: bool) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Software updates · Phorminx").open(&mut open).default_width(460.0).show(ctx,|ui|{
            ui.heading("Keep your instrument current");
            ui.label(format!("Installed version: {}",updates::current_version()));
            ui.label("Only signed stable releases are accepted. Installation waits until you close Phorminx; recording is never stopped for an update.");
            let configured=updates::configured_key().is_ok();
            let previous=self.preferences.automatic;
            if ui.add_enabled(configured&&!self.scheduled,egui::Checkbox::new(&mut self.preferences.automatic,"Automatically download and install on exit")).changed() {
                if let Err(error)=self.persist(){self.preferences.automatic=previous;self.notice=error;}
                else if self.preferences.automatic {self.next_check=Instant::now();}
            }
            ui.horizontal(|ui|{
                if ui.add_enabled(self.pending.is_none()&&!self.scheduled,egui::Button::new("Check for updates")).clicked(){self.check();}
                if self.release.is_some()&&self.staged.is_none()&&ui.add_enabled(self.pending.is_none(),egui::Button::new("Download update")).clicked(){self.download();}
                if self.staged.is_some()&&ui.add_enabled(idle&&!self.scheduled,egui::Button::new("Install after closing")).clicked(){self.schedule();}
            });
            if !configured {ui.weak("This development build has no production release-signing key. Automatic delivery is disabled.");}
            if !self.notice.is_empty(){ui.separator();ui.label(&self.notice);}
        });
        self.open = open;
        if self.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod qa_tests {
    use super::*;

    fn release(sequence: u64) -> Release {
        Release {
            schema_version: 1,
            version: "1.2.0".into(),
            sequence,
            channel: "stable".into(),
            platform: "windows-x64".into(),
            published_at: 1000,
            expires_at: 2000,
            download_url:
                "https://github.com/impossibleG/phorminx/releases/download/v1.2.0/setup.exe".into(),
            sha256: "a".repeat(64),
            bytes: 100,
        }
    }

    fn deliver(center: &mut UpdateCenter, event: ResultEvent) {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(event).unwrap();
        center.pending = Some(rx);
        // Non-idle prevents any network/installer worker from being started.
        center.poll(false);
    }

    #[test]
    fn qa_current_release_still_persists_floor_and_invalidates_old_package() {
        let directory = tempfile::tempdir().unwrap();
        let mut center = UpdateCenter::new(directory.path().to_owned());
        center.release = Some(release(9));
        center.staged = Some(directory.path().join("old.exe"));
        deliver(
            &mut center,
            ResultEvent::Checked(Ok(updates::VerifiedCheck {
                release: None,
                highest_sequence: 20,
            })),
        );
        assert!(center.release.is_none());
        assert!(center.staged.is_none());
        assert_eq!(
            UpdateCenter::new(directory.path().to_owned())
                .preferences
                .highest_sequence,
            20
        );
    }

    #[test]
    fn qa_transient_failure_retains_automatic_consent_and_backs_off() {
        let directory = tempfile::tempdir().unwrap();
        let mut center = UpdateCenter::new(directory.path().to_owned());
        center.preferences.automatic = true;
        deliver(
            &mut center,
            ResultEvent::Checked(Err("synthetic outage".into())),
        );
        assert!(center.preferences.automatic);
        assert!(center.retry_after > Instant::now() + Duration::from_secs(14 * 60));
        assert!(center.pending.is_none());
    }

    #[test]
    fn qa_changed_release_clears_staged_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut center = UpdateCenter::new(directory.path().to_owned());
        center.release = Some(release(9));
        center.staged = Some(directory.path().join("old.exe"));
        deliver(
            &mut center,
            ResultEvent::Checked(Ok(updates::VerifiedCheck {
                release: Some(release(10)),
                highest_sequence: 10,
            })),
        );
        assert!(center.staged.is_none());
        assert_eq!(center.release.unwrap().sequence, 10);
    }

    #[test]
    fn qa_floor_storage_failure_must_not_leave_older_release_actionable() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("not-a-directory");
        std::fs::write(&blocked, b"synthetic storage failure").unwrap();
        let mut center = UpdateCenter::new(blocked);
        center.release = Some(release(9));
        center.staged = Some(directory.path().join("old.exe"));
        deliver(
            &mut center,
            ResultEvent::Checked(Ok(updates::VerifiedCheck {
                release: Some(release(10)),
                highest_sequence: 10,
            })),
        );
        assert!(
            center.release.is_none(),
            "Older verified release must not remain actionable after receiving a newer floor that cannot be persisted"
        );
        assert!(center.staged.is_none());
    }
}
