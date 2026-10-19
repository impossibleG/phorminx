//! Background update checks and explicit install-on-exit consent.
use crate::updates::{self, Release};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn daily_checks_by_default() -> bool { true }

const IDLE_RESTART_AFTER: Duration = Duration::from_secs(30 * 60);

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Preferences {
    #[serde(default = "daily_checks_by_default")]
    check_daily: bool,
    automatic: bool,
    highest_sequence: u64,
    last_successful_check: u64,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { check_daily: true, automatic: true, highest_sequence: 0, last_successful_check: 0 }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    pub installed_version: String,
    pub daily_checks: bool,
    pub automatic_install: bool,
    pub configured: bool,
    pub busy: bool,
    pub available_version: Option<String>,
    pub downloaded: bool,
    pub scheduled: bool,
    pub last_successful_check: Option<u64>,
    pub notice: String,
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
        let preferences: Preferences = std::fs::read(directory.join("preferences.json"))
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
        // A relaunch request belongs to the process that wrote it; never let a
        // stale one reopen Phorminx after an ordinary later exit.
        let _ = std::fs::remove_file(updates::relaunch_flag(&directory.join("packages")));
        let now = unix_now();
        let due_in = (preferences.last_successful_check.saturating_add(24 * 60 * 60))
            .saturating_sub(now);
        Self {
            open: false,
            directory,
            preferences,
            next_check: Instant::now() + Duration::from_secs(due_in.min(24 * 60 * 60).max(30 + jitter)),
            pending: None,
            release: None,
            staged: None,
            notice,
            scheduled: false,
            retry_after: Instant::now(),
        }
    }
    pub fn snapshot(&self) -> UpdateView {
        UpdateView {
            installed_version: updates::current_version().to_owned(),
            daily_checks: self.preferences.check_daily,
            automatic_install: self.preferences.automatic,
            configured: updates::configured_key().is_ok(),
            busy: self.pending.is_some(),
            available_version: self.release.as_ref().map(|release| release.version.clone()),
            downloaded: self.staged.is_some(),
            scheduled: self.scheduled,
            last_successful_check: (self.preferences.last_successful_check != 0)
                .then_some(self.preferences.last_successful_check),
            notice: self.notice.clone(),
        }
    }
    pub fn set_daily_checks(&mut self, enabled: bool) -> Result<(), String> {
        let previous = self.preferences.check_daily;
        self.preferences.check_daily = enabled;
        if let Err(error) = self.persist() {
            self.preferences.check_daily = previous;
            return Err(error);
        }
        if enabled { self.next_check = Instant::now(); }
        Ok(())
    }
    pub fn set_automatic_install(&mut self, enabled: bool) -> Result<(), String> {
        if enabled && updates::configured_key().is_err() {
            return Err("This build has no trusted release-signing key.".into());
        }
        if self.scheduled { return Err("An update is already scheduled.".into()); }
        let previous = self.preferences.automatic;
        self.preferences.automatic = enabled;
        if let Err(error) = self.persist() {
            self.preferences.automatic = previous;
            return Err(error);
        }
        if enabled { self.next_check = Instant::now(); }
        Ok(())
    }
    pub fn check_now(&mut self) { self.check(); }
    pub fn download_now(&mut self) { self.download(); }
    pub fn install_on_exit(&mut self, idle: bool) -> Result<(), String> {
        if !idle { return Err("Finish the active recording or meeting before scheduling installation.".into()); }
        if self.pending.is_some() { return Err("Wait for the current update operation to finish.".into()); }
        if self.staged.is_none() || self.release.is_none() {
            return Err("Download and verify an update first.".into());
        }
        self.schedule();
        if self.scheduled { Ok(()) } else { Err(self.notice.clone()) }
    }
    /// Schedule the verified update and ask the install helper to reopen
    /// Phorminx. The caller must then exit the app.
    pub fn restart_to_update(&mut self, idle: bool, background: bool) -> Result<(), String> {
        if !idle { return Err("Finish the active recording or meeting before restarting to update.".into()); }
        if self.pending.is_some() { return Err("Wait for the current update operation to finish.".into()); }
        let Some(staged) = self.staged.as_ref() else {
            return Err("Download and verify an update first.".into());
        };
        let flag = updates::relaunch_flag(staged.parent().ok_or("The update package is unavailable.")?);
        std::fs::write(&flag, if background { "background" } else { "window" })
            .map_err(|_| "Could not prepare the restart.")?;
        if !self.scheduled { self.schedule(); }
        if self.scheduled {
            Ok(())
        } else {
            let _ = std::fs::remove_file(flag);
            Err(self.notice.clone())
        }
    }
    /// A tray app that starts with Windows rarely exits, so a ready automatic
    /// update restarts it once the user has been away for a while.
    pub fn idle_restart_due(&self, user_idle: Duration) -> bool {
        self.preferences.automatic
            && self.staged.is_some()
            && self.pending.is_none()
            && user_idle >= IDLE_RESTART_AFTER
    }
    fn persist(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory)
            .map_err(|_| "Could not save update preferences.")?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Could not save update preferences.")?
            .as_nanos();
        let temporary = self.directory.join(format!("preferences-{}-{nonce}.tmp", std::process::id()));
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
            self.next_check = Instant::now() + Duration::from_secs(24 * 60 * 60);
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
        self.next_check = Instant::now() + Duration::from_secs(24 * 60 * 60);
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
                            self.preferences.last_successful_check = unix_now();
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
                        ResultEvent::Checked(Err(error)) => {
                            self.notice = error;
                            self.retry_after = Instant::now() + Duration::from_secs(15 * 60);
                            self.next_check = self.retry_after;
                        }
                        ResultEvent::Staged(Err(error)) => {
                            self.notice = error;
                            self.retry_after = Instant::now() + Duration::from_secs(15 * 60);
                        }
                        ResultEvent::Staged(Ok(path)) => {
                            self.staged = Some(path);
                            self.notice =
                                "Verified update ready. Restart Phorminx to finish updating."
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
        if idle
            && self.pending.is_none()
            && !self.scheduled
            && Instant::now() >= self.retry_after
        {
            if self.preferences.check_daily && Instant::now() >= self.next_check {
                self.check();
            } else if self.preferences.automatic && self.staged.is_some() {
                self.schedule();
            } else if self.preferences.automatic && self.release.is_some() && self.staged.is_none() {
                self.download();
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
                    "Update installs when Phorminx restarts or closes. Recording will not be interrupted."
                        .into();
            }
            Err(error) => {
                self.notice = error.to_string();
                self.retry_after = Instant::now() + Duration::from_secs(15 * 60);
            }
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, idle: bool) {
        self.poll(idle);
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Software updates · Phorminx").open(&mut open).default_width(460.0).show(ctx,|ui|{
            ui.heading("Keep your instrument current");
            ui.label(format!("Installed version: {}",updates::current_version()));
            ui.label("Only signed stable releases are accepted. Installation waits until you close Phorminx; recording is never stopped for an update.");
            let configured=updates::configured_key().is_ok();
            let mut daily=self.preferences.check_daily;
            if ui.checkbox(&mut daily,"Check for signed releases daily").changed()
                && let Err(error)=self.set_daily_checks(daily) {self.notice=error;}
            let mut automatic=self.preferences.automatic;
            if ui.add_enabled(configured&&!self.scheduled,egui::Checkbox::new(&mut automatic,"Automatically download and install on exit")).changed()
                && let Err(error)=self.set_automatic_install(automatic){self.notice=error;}
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

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
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

    #[test]
    fn qa_restart_to_update_requires_idle_and_a_verified_package() {
        let directory = tempfile::tempdir().unwrap();
        let mut center = UpdateCenter::new(directory.path().to_owned());
        assert!(center.restart_to_update(false, false).is_err());
        assert!(center.restart_to_update(true, false).is_err());
        assert!(!center.scheduled);
    }

    #[test]
    fn qa_idle_restart_waits_for_automatic_consent_package_and_absence() {
        let directory = tempfile::tempdir().unwrap();
        let mut center = UpdateCenter::new(directory.path().to_owned());
        let away = IDLE_RESTART_AFTER;
        assert!(!center.idle_restart_due(away), "nothing staged");
        center.staged = Some(directory.path().join("package.exe"));
        assert!(center.idle_restart_due(away));
        assert!(!center.idle_restart_due(away - Duration::from_secs(1)), "user still active");
        center.preferences.automatic = false;
        assert!(!center.idle_restart_due(away), "automatic install is off");
    }

    #[test]
    fn qa_stale_relaunch_request_is_cleared_on_start() {
        let directory = tempfile::tempdir().unwrap();
        let packages = directory.path().join("packages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(updates::relaunch_flag(&packages), b"window").unwrap();
        let _center = UpdateCenter::new(directory.path().to_owned());
        assert!(!updates::relaunch_flag(&packages).exists());
    }

    #[test]
    fn qa_daily_checks_and_automatic_install_default_on() {
        let directory = tempfile::tempdir().unwrap();
        let center = UpdateCenter::new(directory.path().to_owned());
        let view = center.snapshot();
        assert!(view.daily_checks);
        assert!(view.automatic_install);
        assert!(view.last_successful_check.is_none());
    }

    #[test]
    fn qa_old_preferences_gain_daily_checks_without_losing_rollback_floor() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("preferences.json"), br#"{"automatic":false,"highest_sequence":42}"#).unwrap();
        let center = UpdateCenter::new(directory.path().to_owned());
        assert!(center.preferences.check_daily);
        assert!(!center.preferences.automatic);
        assert_eq!(center.preferences.highest_sequence, 42);
    }

    #[test]
    fn qa_daily_check_preference_persists_independently_of_auto_install() {
        let directory = tempfile::tempdir().unwrap();
        let mut center = UpdateCenter::new(directory.path().to_owned());
        center.set_daily_checks(false).unwrap();
        let reopened = UpdateCenter::new(directory.path().to_owned());
        assert!(!reopened.preferences.check_daily);
        assert!(reopened.preferences.automatic);
    }

    #[test]
    fn qa_auto_install_choice_reopens_from_the_shared_preferences_file() {
        let directory = tempfile::tempdir().unwrap();
        let mut center = UpdateCenter::new(directory.path().to_owned());
        center.set_automatic_install(false).unwrap();
        let reopened = UpdateCenter::new(directory.path().to_owned());
        assert!(!reopened.snapshot().automatic_install);
        assert!(reopened.snapshot().daily_checks);
    }
}
