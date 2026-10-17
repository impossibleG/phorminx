//! Investigations: long-lived, local collections of captured material.
//!
//! The service owns the UI database connection, the image folder and the
//! background workers (screenshot OCR and link previews). It is independent of
//! Tauri so the behavior is covered by ordinary tests; `tauri_investigations`
//! only adapts it to commands.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::imageops::FilterType;
use phorminx_persistence::{
    EdgeStatus, ImageReference, InvestigationItemKind, InvestigationItemRecord,
    InvestigationMessageStatus, NewInvestigationItem, Persistence, WorkState,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::investigation_links::{LinkWorker, parse_link};
use crate::investigation_ocr::{
    OCR_MODEL_DIRECTORY, OcrModelManager, OcrModelView, OcrWorker, WorkerSignal, now_ms,
};

/// Largest encoded image accepted from paste, drop or the clipboard.
pub const MAX_IMAGE_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub const IMAGE_FOLDER: &str = "investigations";
const NEW_NODE_OFFSET_X: f64 = 300.0;
const NEW_NODE_STEP_Y: f64 = 150.0;

pub struct StoredImage {
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

impl StoredImage {
    pub fn into_reference(self) -> ImageReference {
        ImageReference {
            sha256: self.sha256,
            width: self.width,
            height: self.height,
        }
    }
}

/// Decodes untrusted image bytes, optionally downsizes them, and stores a
/// clean PNG named by its SHA-256 in `folder`. Re-encoding drops metadata
/// such as EXIF location and embedded text chunks.
pub fn store_image(
    folder: &Path,
    bytes: &[u8],
    max_side: Option<u32>,
) -> Result<StoredImage, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_INPUT_BYTES {
        return Err("Images up to 32 MB can be added.".into());
    }
    let decoded = phorminx_ocr::decode_image(bytes).map_err(|error| match error {
        phorminx_ocr::OcrError::TooLarge => "This image has too many pixels to add.".to_owned(),
        _ => "This file is not a supported image (PNG, JPEG, GIF, BMP, WebP).".to_owned(),
    })?;
    let decoded = match max_side {
        Some(limit) if decoded.width().max(decoded.height()) > limit => {
            let scale = limit as f32 / decoded.width().max(decoded.height()) as f32;
            image::imageops::resize(
                &decoded,
                ((decoded.width() as f32 * scale).round() as u32).max(1),
                ((decoded.height() as f32 * scale).round() as u32).max(1),
                FilterType::Triangle,
            )
        }
        _ => decoded,
    };
    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgb8(decoded.clone())
        .write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .map_err(|_| "The image could not be saved.")?;
    let sha256 = hex_sha256(&encoded);
    fs::create_dir_all(folder).map_err(|_| "The investigation folder could not be created.")?;
    let path = folder.join(format!("{sha256}.png"));
    if !path.is_file() {
        let temporary = folder.join(format!(".{sha256}.{}.tmp", std::process::id()));
        fs::write(&temporary, &encoded).map_err(|_| "The image could not be saved.")?;
        if fs::rename(&temporary, &path).is_err() {
            let _ = fs::remove_file(&temporary);
            if !path.is_file() {
                return Err("The image could not be saved.".into());
            }
        }
    }
    Ok(StoredImage {
        sha256,
        width: decoded.width(),
        height: decoded.height(),
    })
}

/// Link preview images and favicons are stored the same way, downsized.
pub fn store_preview_image(
    folder: &Path,
    bytes: &[u8],
    max_side: u32,
) -> Result<StoredImage, String> {
    store_image(folder, bytes, Some(max_side))
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationSummaryView {
    pub id: i64,
    pub title: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub item_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationsView {
    pub revision: u64,
    pub investigations: Vec<InvestigationSummaryView>,
    pub active_id: Option<i64>,
    pub ocr: OcrModelView,
    pub pending_ocr: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemView {
    pub id: i64,
    pub kind: &'static str,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub link_title: Option<String>,
    pub link_description: Option<String>,
    pub link_site: Option<String>,
    pub image_sha: Option<String>,
    pub image_width: u32,
    pub image_height: u32,
    pub favicon_sha: Option<String>,
    pub ocr_state: Option<&'static str>,
    pub fetch_state: Option<&'static str>,
    pub source: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub node_id: Option<i64>,
}

impl ItemView {
    fn from_record(item: InvestigationItemRecord, nodes: &HashMap<i64, i64>) -> Self {
        Self {
            node_id: nodes.get(&item.id).copied(),
            id: item.id,
            kind: item.kind.as_str(),
            title: item.title,
            body: item.body,
            url: item.url,
            link_title: item.link_title,
            link_description: item.link_description,
            link_site: item.link_site,
            image_width: item.image.as_ref().map_or(0, |image| image.width),
            image_height: item.image.as_ref().map_or(0, |image| image.height),
            image_sha: item.image.map(|image| image.sha256),
            favicon_sha: item.favicon_sha256,
            ocr_state: item.ocr_state.map(WorkState::as_str),
            fetch_state: item.fetch_state.map(WorkState::as_str),
            source: item.source,
            created_at_ms: item.created_at_ms,
            updated_at_ms: item.updated_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: i64,
    pub item_id: Option<i64>,
    pub label: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeView {
    pub id: i64,
    pub from: i64,
    pub to: i64,
    pub label: String,
    pub suggested: bool,
    pub rationale: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub role: String,
    pub status: &'static str,
    pub text: String,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationDetailView {
    pub id: i64,
    pub title: String,
    pub revision: u64,
    pub items: Vec<ItemView>,
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
    pub messages: Vec<MessageView>,
    pub layout: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLineView {
    pub text: String,
    pub confidence: f32,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// Clipboard contents handed to quick capture.
pub enum ClipboardCapture {
    Text(String),
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickCaptureOutcome {
    pub investigation_id: i64,
    pub investigation_title: String,
    pub kind: &'static str,
}

/// What a capture should be attached to on the diagram, if anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attachment {
    pub parent_node: Option<i64>,
}

pub struct InvestigationService {
    images: PathBuf,
    db: Mutex<Persistence>,
    revision: Arc<AtomicU64>,
    ocr_signal: Arc<WorkerSignal>,
    link_signal: Arc<WorkerSignal>,
    models: Arc<OcrModelManager>,
    workers: Mutex<Option<(OcrWorker, LinkWorker)>>,
}

impl InvestigationService {
    /// Opens storage in the Phorminx data folder (the settings directory).
    pub fn open(data_directory: &Path) -> Result<Self, String> {
        let database = data_directory.join("phorminx.db");
        let db = Persistence::open_with_busy_timeout(&database, Duration::from_secs(2))
            .map_err(|_| "Investigation storage could not open.")?;
        let _ = db.investigations().fail_interrupted_messages(now_ms());
        let revision = Arc::new(AtomicU64::new(1));
        let notify_revision = revision.clone();
        let models = Arc::new(OcrModelManager::new(
            data_directory.join(OCR_MODEL_DIRECTORY),
            Arc::new(move || {
                notify_revision.fetch_add(1, Ordering::AcqRel);
            }),
        ));
        Ok(Self {
            images: data_directory.join(IMAGE_FOLDER),
            db: Mutex::new(db),
            revision,
            ocr_signal: Arc::new(WorkerSignal::default()),
            link_signal: Arc::new(WorkerSignal::default()),
            models,
            workers: Mutex::new(None),
        })
    }

    /// Starts OCR and link-preview workers. Tests that only exercise storage
    /// leave them off.
    pub fn start_workers(&self, data_directory: &Path) -> Result<(), String> {
        let database = data_directory.join("phorminx.db");
        let ocr = OcrWorker::spawn(
            database.clone(),
            self.images.clone(),
            self.models.clone(),
            self.ocr_signal.clone(),
            self.revision.clone(),
        )?;
        let links = LinkWorker::spawn(
            database,
            self.images.clone(),
            self.link_signal.clone(),
            self.revision.clone(),
        )?;
        *self
            .workers
            .lock()
            .map_err(|_| "Investigation workers are unavailable.")? = Some((ocr, links));
        Ok(())
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub fn bump(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub fn models(&self) -> &OcrModelManager {
        &self.models
    }

    /// Pauses OCR while dictation is listening or transcribing.
    pub fn set_capture_busy(&self, busy: bool) {
        self.ocr_signal
            .set_paused(crate::investigation_ocr::PAUSE_DICTATION, busy);
    }

    /// Pauses OCR while a voice note is being recorded and transcribed.
    pub fn set_voice_busy(&self, busy: bool) {
        self.ocr_signal
            .set_paused(crate::investigation_ocr::PAUSE_VOICE_NOTE, busy);
    }

    pub fn install_ocr_models(&self) -> Result<(), String> {
        self.models.install()?;
        self.ocr_signal.notify();
        self.bump();
        Ok(())
    }

    pub fn remove_ocr_models(&self) -> Result<(), String> {
        self.models.remove()?;
        self.bump();
        Ok(())
    }

    pub(crate) fn db(&self) -> Result<MutexGuard<'_, Persistence>, String> {
        self.db
            .lock()
            .map_err(|_| "Investigation storage is unavailable.".to_owned())
    }

    pub fn images_folder(&self, investigation_id: i64) -> PathBuf {
        self.images.join(investigation_id.to_string())
    }

    pub fn overview(&self) -> Result<InvestigationsView, String> {
        let db = self.db()?;
        let repo = db.investigations();
        Ok(InvestigationsView {
            revision: self.revision(),
            investigations: repo
                .list()
                .map_err(|_| "Investigations could not be listed.")?
                .into_iter()
                .map(|record| InvestigationSummaryView {
                    id: record.id,
                    title: record.title,
                    created_at_ms: record.created_at_ms,
                    updated_at_ms: record.updated_at_ms,
                    item_count: record.item_count,
                })
                .collect(),
            active_id: repo.active().unwrap_or(None),
            ocr: self.models.view(),
            pending_ocr: repo.pending_ocr_count().unwrap_or(0),
        })
    }

    pub fn create(&self, title: &str) -> Result<i64, String> {
        let db = self.db()?;
        let repo = db.investigations();
        let id = repo
            .create(title, now_ms())
            .map_err(|_| "The investigation could not be created.")?;
        if repo.active().unwrap_or(None).is_none() {
            let _ = repo.set_active(Some(id));
        }
        drop(db);
        self.bump();
        Ok(id)
    }

    pub fn rename(&self, id: i64, title: &str) -> Result<(), String> {
        let changed = self
            .db()?
            .investigations()
            .rename(id, title, now_ms())
            .map_err(|_| "The investigation could not be renamed.")?;
        if !changed {
            return Err("Choose an existing investigation.".into());
        }
        self.bump();
        Ok(())
    }

    /// Deletes the investigation, its rows and its image folder.
    pub fn delete(&self, id: i64) -> Result<(), String> {
        let deleted = self
            .db()?
            .investigations()
            .delete(id)
            .map_err(|_| "The investigation could not be deleted.")?;
        if !deleted {
            return Err("Choose an existing investigation.".into());
        }
        let folder = self.images_folder(id);
        if folder.exists() && fs::remove_dir_all(&folder).is_err() {
            self.bump();
            return Err("The investigation was deleted, but some image files are still in use. They will be removed the next time you delete it or restart Phorminx.".into());
        }
        self.bump();
        Ok(())
    }

    /// Removes leftover image folders whose investigation no longer exists.
    pub fn remove_orphaned_folders(&self) {
        let Ok(entries) = fs::read_dir(&self.images) else {
            return;
        };
        let Ok(db) = self.db() else {
            return;
        };
        for entry in entries.flatten() {
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i64>().ok())
            else {
                continue;
            };
            if db
                .investigations()
                .get(id)
                .is_ok_and(|record| record.is_none())
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    pub fn set_active(&self, id: Option<i64>) -> Result<(), String> {
        self.db()?
            .investigations()
            .set_active(id)
            .map_err(|_| "Choose an existing investigation.")?;
        self.bump();
        Ok(())
    }

    pub fn active(&self) -> Result<Option<i64>, String> {
        self.db()?
            .investigations()
            .active()
            .map_err(|_| "Investigations are unavailable.".to_owned())
    }

    /// The active investigation, creating "Quick captures" when there is none.
    pub fn active_or_create(&self) -> Result<i64, String> {
        if let Some(id) = self.active()? {
            return Ok(id);
        }
        let id = self.create("Quick captures")?;
        self.set_active(Some(id))?;
        Ok(id)
    }

    pub fn title(&self, id: i64) -> Result<String, String> {
        self.db()?
            .investigations()
            .get(id)
            .ok()
            .flatten()
            .map(|record| record.title)
            .ok_or_else(|| "Choose an existing investigation.".to_owned())
    }

    pub fn detail(
        &self,
        id: i64,
        since: Option<u64>,
    ) -> Result<Option<InvestigationDetailView>, String> {
        let revision = self.revision();
        if since == Some(revision) {
            return Ok(None);
        }
        let db = self.db()?;
        let repo = db.investigations();
        let record = repo
            .get(id)
            .map_err(|_| "The investigation could not be opened.")?
            .ok_or("This investigation no longer exists.")?;
        let nodes = repo
            .nodes(id)
            .map_err(|_| "The diagram could not be read.")?;
        let placed: HashMap<i64, i64> = nodes
            .iter()
            .filter_map(|node| node.item_id.map(|item| (item, node.id)))
            .collect();
        let items = repo
            .items(id, None, None)
            .map_err(|_| "Items could not be read.")?
            .into_iter()
            .map(|item| ItemView::from_record(item, &placed))
            .collect();
        Ok(Some(InvestigationDetailView {
            id,
            title: record.title,
            revision,
            items,
            nodes: nodes
                .into_iter()
                .map(|node| NodeView {
                    id: node.id,
                    item_id: node.item_id,
                    label: node.label,
                    x: node.x,
                    y: node.y,
                })
                .collect(),
            edges: repo
                .edges(id)
                .map_err(|_| "The diagram could not be read.")?
                .into_iter()
                .map(|edge| EdgeView {
                    id: edge.id,
                    from: edge.from_node,
                    to: edge.to_node,
                    label: edge.label,
                    suggested: edge.status == EdgeStatus::Suggested,
                    rationale: edge.rationale,
                })
                .collect(),
            messages: repo
                .messages(id, 200)
                .map_err(|_| "Assistant messages could not be read.")?
                .into_iter()
                .map(|message| MessageView {
                    id: message.id,
                    role: message.role,
                    status: message.status.as_str(),
                    text: message.text,
                    created_at_ms: message.created_at_ms,
                })
                .collect(),
            layout: repo.layout(id).unwrap_or_else(|_| "{}".into()),
        }))
    }

    /// Item ids matching an exact (case-insensitive) phrase and/or a kind.
    pub fn search(&self, id: i64, query: &str, kind: Option<&str>) -> Result<Vec<i64>, String> {
        let kind = match kind.filter(|kind| !kind.is_empty() && *kind != "all") {
            Some(kind) => {
                Some(InvestigationItemKind::parse(kind).ok_or("Choose a supported card type.")?)
            }
            None => None,
        };
        if query.chars().count() > 300 {
            return Err("Search is limited to 300 characters.".into());
        }
        Ok(self
            .db()?
            .investigations()
            .items(id, kind, Some(query))
            .map_err(|_| "Search failed.")?
            .into_iter()
            .map(|item| item.id)
            .collect())
    }

    /// Adds typed or pasted text. A lone URL becomes a link card.
    pub fn add_text(
        &self,
        investigation_id: i64,
        text: &str,
        typed_note: bool,
        source: &str,
        attachment: Attachment,
    ) -> Result<i64, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("Type or paste some text first.".into());
        }
        if trimmed.len() > phorminx_persistence::MAX_ITEM_BODY_BYTES {
            return Err("Text cards are limited to 256 KB.".into());
        }
        let item = match parse_link(trimmed) {
            Some(url) => NewInvestigationItem {
                url: Some(url.to_string()),
                source: source.into(),
                ..NewInvestigationItem::new(InvestigationItemKind::Link, "")
            },
            None => NewInvestigationItem {
                source: source.into(),
                ..NewInvestigationItem::new(
                    if typed_note {
                        InvestigationItemKind::Note
                    } else {
                        InvestigationItemKind::Text
                    },
                    trimmed,
                )
            },
        };
        let is_link = item.kind == InvestigationItemKind::Link;
        let id = self.add_item(investigation_id, &item, attachment)?;
        if is_link {
            self.link_signal.notify();
        }
        Ok(id)
    }

    pub fn add_voice_note(
        &self,
        investigation_id: i64,
        text: &str,
        attachment: Attachment,
    ) -> Result<i64, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("No speech was captured.".into());
        }
        self.add_item(
            investigation_id,
            &NewInvestigationItem {
                source: "voice".into(),
                ..NewInvestigationItem::new(InvestigationItemKind::Voice, text)
            },
            attachment,
        )
    }

    /// Adds an encoded image (paste, drop or clipboard) and queues OCR.
    pub fn add_image(
        &self,
        investigation_id: i64,
        bytes: &[u8],
        source: &str,
        attachment: Attachment,
    ) -> Result<i64, String> {
        self.title(investigation_id)?;
        let stored = store_image(&self.images_folder(investigation_id), bytes, None)?;
        let id = self.add_item(
            investigation_id,
            &NewInvestigationItem {
                image: Some(stored.into_reference()),
                source: source.into(),
                ..NewInvestigationItem::new(InvestigationItemKind::Image, "")
            },
            attachment,
        )?;
        self.ocr_signal.notify();
        Ok(id)
    }

    /// Adds base64 image data sent by the window (`data:` URLs accepted).
    pub fn add_image_base64(
        &self,
        investigation_id: i64,
        data: &str,
        source: &str,
        attachment: Attachment,
    ) -> Result<i64, String> {
        let payload = data.split_once(',').map_or(data, |(prefix, rest)| {
            if prefix.starts_with("data:") {
                rest
            } else {
                data
            }
        });
        if payload.len() > MAX_IMAGE_INPUT_BYTES / 3 * 4 + 8 {
            return Err("Images up to 32 MB can be added.".into());
        }
        let bytes = STANDARD
            .decode(payload.trim())
            .map_err(|_| "The image data could not be read.")?;
        self.add_image(investigation_id, &bytes, source, attachment)
    }

    /// Sends clipboard contents into the active investigation (creating
    /// "Quick captures" if none is active).
    pub fn quick_capture(&self, capture: ClipboardCapture) -> Result<QuickCaptureOutcome, String> {
        let investigation_id = self.active_or_create()?;
        self.capture_into(investigation_id, capture)
    }

    /// Adds clipboard contents to a specific investigation (the window's
    /// "Add clipboard" button).
    pub fn capture_into(
        &self,
        investigation_id: i64,
        capture: ClipboardCapture,
    ) -> Result<QuickCaptureOutcome, String> {
        self.title(investigation_id)?;
        let kind = match capture {
            ClipboardCapture::Text(text) => {
                let id = self.add_text(
                    investigation_id,
                    &text,
                    false,
                    "quick-capture",
                    Attachment::default(),
                )?;
                if self
                    .db()?
                    .investigations()
                    .item(id)
                    .ok()
                    .flatten()
                    .is_some_and(|item| item.kind == InvestigationItemKind::Link)
                {
                    "link"
                } else {
                    "text"
                }
            }
            ClipboardCapture::Image {
                width,
                height,
                rgba,
            } => {
                let image = image::RgbaImage::from_raw(width, height, rgba)
                    .ok_or("The clipboard image could not be read.")?;
                let mut bytes = Vec::new();
                image::DynamicImage::ImageRgba8(image)
                    .write_to(
                        &mut std::io::Cursor::new(&mut bytes),
                        image::ImageFormat::Png,
                    )
                    .map_err(|_| "The clipboard image could not be read.")?;
                self.add_image(
                    investigation_id,
                    &bytes,
                    "quick-capture",
                    Attachment::default(),
                )?;
                "image"
            }
        };
        Ok(QuickCaptureOutcome {
            investigation_title: self.title(investigation_id)?,
            investigation_id,
            kind,
        })
    }

    pub fn pin_answer(&self, message_id: i64) -> Result<i64, String> {
        let (investigation_id, message) = self
            .db()?
            .investigations()
            .message(message_id)
            .map_err(|_| "The answer could not be read.")?
            .ok_or("This answer no longer exists.")?;
        if message.role != "assistant" || message.status != InvestigationMessageStatus::Complete {
            return Err("Only finished assistant answers can be pinned.".into());
        }
        self.add_item(
            investigation_id,
            &NewInvestigationItem {
                source: "assistant".into(),
                ..NewInvestigationItem::new(InvestigationItemKind::Answer, message.text)
            },
            Attachment::default(),
        )
    }

    fn add_item(
        &self,
        investigation_id: i64,
        item: &NewInvestigationItem,
        attachment: Attachment,
    ) -> Result<i64, String> {
        let db = self.db()?;
        let repo = db.investigations();
        let parent = match attachment.parent_node {
            Some(node) => {
                if repo.node_owner(node).ok().flatten() != Some(investigation_id) {
                    return Err("Choose a node in this investigation.".into());
                }
                repo.nodes(investigation_id)
                    .ok()
                    .and_then(|nodes| nodes.into_iter().find(|candidate| candidate.id == node))
            }
            None => None,
        };
        let id = repo
            .add_item(investigation_id, item, now_ms())
            .map_err(|error| match error {
                phorminx_persistence::PersistenceError::CollectionLimitExceeded { .. } => {
                    "This investigation is full (5,000 cards). Start a new one.".to_owned()
                }
                _ => "The card could not be saved.".to_owned(),
            })?;
        if let Some(parent) = parent {
            let siblings = repo
                .edges(investigation_id)
                .map(|edges| {
                    edges
                        .iter()
                        .filter(|edge| edge.from_node == parent.id)
                        .count()
                })
                .unwrap_or(0) as f64;
            let child = repo
                .place_item(
                    id,
                    parent.x + NEW_NODE_OFFSET_X,
                    parent.y + siblings * NEW_NODE_STEP_Y,
                    now_ms(),
                )
                .map_err(|_| "The card was saved but could not be placed on the diagram.")?;
            let _ = repo.add_edge(parent.id, child, EdgeStatus::Accepted, "", "", now_ms());
        }
        drop(db);
        self.bump();
        Ok(id)
    }

    pub fn update_item(&self, item_id: i64, title: &str, body: &str) -> Result<(), String> {
        if body.len() > phorminx_persistence::MAX_ITEM_BODY_BYTES {
            return Err("Card text is limited to 256 KB.".into());
        }
        let changed = self
            .db()?
            .investigations()
            .update_item_text(item_id, title, body, now_ms())
            .map_err(|_| "The card could not be saved.")?;
        if !changed {
            return Err("This card no longer exists.".into());
        }
        self.bump();
        Ok(())
    }

    /// Deletes an item and any image files only it referenced.
    pub fn delete_item(&self, item_id: i64) -> Result<(), String> {
        let (investigation_id, orphaned) = self
            .db()?
            .investigations()
            .delete_item(item_id, now_ms())
            .map_err(|_| "The card could not be deleted.")?
            .ok_or("This card no longer exists.")?;
        let folder = self.images_folder(investigation_id);
        for sha in orphaned.iter().filter(|sha| valid_sha(sha)) {
            let _ = fs::remove_file(folder.join(format!("{sha}.png")));
        }
        self.bump();
        Ok(())
    }

    pub fn retry(&self, item_id: i64) -> Result<(), String> {
        let db = self.db()?;
        let repo = db.investigations();
        let retried = repo.retry_ocr(item_id, now_ms()).unwrap_or(false)
            || repo.retry_fetch(item_id, now_ms()).unwrap_or(false);
        drop(db);
        if !retried {
            return Err("Nothing to retry for this card.".into());
        }
        self.ocr_signal.notify();
        self.link_signal.notify();
        self.bump();
        Ok(())
    }

    /// A stored image as a `data:` URL, only when the investigation uses it.
    pub fn image_data_url(&self, investigation_id: i64, sha: &str) -> Result<String, String> {
        if !valid_sha(sha) {
            return Err("Choose a stored image.".into());
        }
        let known = self
            .db()?
            .investigations()
            .image_hashes(investigation_id)
            .map_err(|_| "Images are unavailable.")?
            .iter()
            .any(|known| known == sha);
        if !known {
            return Err("Choose a stored image.".into());
        }
        let bytes = fs::read(
            self.images_folder(investigation_id)
                .join(format!("{sha}.png")),
        )
        .map_err(|_| "This image file is missing.")?;
        Ok(format!("data:image/png;base64,{}", STANDARD.encode(bytes)))
    }

    pub fn ocr_lines(&self, item_id: i64) -> Result<Vec<OcrLineView>, String> {
        Ok(self
            .db()?
            .investigations()
            .ocr_lines(item_id)
            .map_err(|_| "Extracted lines are unavailable.")?
            .into_iter()
            .map(|line| OcrLineView {
                text: line.text,
                confidence: line.confidence,
                left: line.left,
                top: line.top,
                width: line.width,
                height: line.height,
            })
            .collect())
    }

    pub fn place_item(&self, item_id: i64, x: f64, y: f64) -> Result<i64, String> {
        let id = self
            .db()?
            .investigations()
            .place_item(item_id, x, y, now_ms())
            .map_err(|_| "The card could not be placed.")?;
        self.bump();
        Ok(id)
    }

    pub fn add_concept(
        &self,
        investigation_id: i64,
        label: &str,
        x: f64,
        y: f64,
        attachment: Attachment,
    ) -> Result<i64, String> {
        let db = self.db()?;
        let repo = db.investigations();
        let id = repo
            .add_concept(investigation_id, label, x, y, now_ms())
            .map_err(|_| "The concept could not be added.")?;
        if let Some(parent) = attachment.parent_node {
            if repo.node_owner(parent).ok().flatten() != Some(investigation_id) {
                let _ = repo.delete_node(id);
                return Err("Choose a node in this investigation.".into());
            }
            let _ = repo.add_edge(parent, id, EdgeStatus::Accepted, "", "", now_ms());
        }
        drop(db);
        self.bump();
        Ok(id)
    }

    pub fn move_node(&self, node_id: i64, x: f64, y: f64) -> Result<(), String> {
        let moved = self
            .db()?
            .investigations()
            .move_node(node_id, x, y)
            .map_err(|_| "The node could not be moved.")?;
        if !moved {
            return Err("This node no longer exists.".into());
        }
        self.bump();
        Ok(())
    }

    pub fn rename_node(&self, node_id: i64, label: &str) -> Result<(), String> {
        let renamed = self
            .db()?
            .investigations()
            .rename_node(node_id, label)
            .map_err(|_| "The concept could not be renamed.")?;
        if !renamed {
            return Err(
                "Only concept nodes can be renamed here; edit a card's title in its detail view."
                    .into(),
            );
        }
        self.bump();
        Ok(())
    }

    pub fn delete_node(&self, node_id: i64) -> Result<(), String> {
        self.db()?
            .investigations()
            .delete_node(node_id)
            .map_err(|_| "The node could not be removed.")?;
        self.bump();
        Ok(())
    }

    pub fn connect(&self, from: i64, to: i64) -> Result<(), String> {
        let added = self
            .db()?
            .investigations()
            .add_edge(from, to, EdgeStatus::Accepted, "", "", now_ms())
            .map_err(|_| "These nodes cannot be connected.")?;
        if added.is_none() {
            return Err("These nodes are already connected.".into());
        }
        self.bump();
        Ok(())
    }

    pub fn accept_edge(&self, edge_id: i64) -> Result<(), String> {
        self.db()?
            .investigations()
            .accept_edge(edge_id)
            .map_err(|_| "The connection could not be accepted.")?;
        self.bump();
        Ok(())
    }

    pub fn delete_edge(&self, edge_id: i64) -> Result<(), String> {
        self.db()?
            .investigations()
            .delete_edge(edge_id)
            .map_err(|_| "The connection could not be removed.")?;
        self.bump();
        Ok(())
    }

    pub fn save_layout(&self, investigation_id: i64, layout: &str) -> Result<(), String> {
        self.db()?
            .investigations()
            .save_layout(investigation_id, layout)
            .map_err(|_| "The diagram view could not be saved.")?;
        Ok(())
    }

    pub fn clear_messages(&self, investigation_id: i64) -> Result<(), String> {
        self.db()?
            .investigations()
            .clear_messages(investigation_id)
            .map_err(|_| "The conversation could not be cleared.")?;
        self.bump();
        Ok(())
    }

    /// A self-contained Markdown document of the investigation, in
    /// chronological order, with the diagram's connections listed.
    pub fn export_markdown(&self, investigation_id: i64) -> Result<String, String> {
        let detail = self
            .detail(investigation_id, None)?
            .ok_or("This investigation no longer exists.")?;
        Ok(render_markdown(&detail))
    }
}

impl Drop for InvestigationService {
    fn drop(&mut self) {
        self.ocr_signal.stop();
        self.link_signal.stop();
        if let Ok(mut workers) = self.workers.lock() {
            workers.take();
        }
    }
}

pub fn card_label(item: &ItemView) -> String {
    let source = if !item.title.trim().is_empty() {
        item.title.clone()
    } else if let Some(title) = item.link_title.as_ref().filter(|title| !title.is_empty()) {
        title.clone()
    } else if let Some(url) = &item.url {
        url.clone()
    } else {
        item.body
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .to_owned()
    };
    let mut label: String = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.chars().count() > 80 {
        label = label.chars().take(79).collect::<String>() + "…";
    }
    if label.is_empty() {
        match item.kind {
            "image" => "Screenshot".into(),
            "voice" => "Voice note".into(),
            "link" => "Link".into(),
            _ => "Card".into(),
        }
    } else {
        label
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "text" => "Pasted text",
        "note" => "Note",
        "image" => "Screenshot",
        "link" => "Link",
        "voice" => "Voice note",
        "answer" => "Pinned answer",
        _ => "Card",
    }
}

fn format_time(ms: i64) -> String {
    // UTC keeps exports deterministic and unambiguous across machines.
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time / 3600,
        (time % 3600) / 60
    )
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn quote_block(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_markdown(detail: &InvestigationDetailView) -> String {
    let mut out = format!("# {}\n\n", detail.title.replace('\n', " "));
    out.push_str(&format!(
        "{} cards · exported by Phorminx\n\n## Chronology\n\n",
        detail.items.len()
    ));
    for item in &detail.items {
        out.push_str(&format!(
            "### #{} · {} — {}\n\n*{}*\n\n",
            item.id,
            kind_label(item.kind),
            card_label(item),
            format_time(item.created_at_ms)
        ));
        if let Some(url) = &item.url {
            out.push_str(&format!("<{url}>\n\n"));
            if let Some(description) = &item.link_description {
                out.push_str(&quote_block(description));
                out.push_str("\n\n");
            }
        }
        if item.kind == "image" {
            if let Some(sha) = &item.image_sha {
                out.push_str(&format!(
                    "Screenshot `investigations/{}/{sha}.png` ({}×{})\n\n",
                    detail.id, item.image_width, item.image_height
                ));
            }
            if !item.body.trim().is_empty() {
                out.push_str("Extracted text:\n\n");
            }
        }
        if !item.body.trim().is_empty() {
            out.push_str(&quote_block(item.body.trim()));
            out.push_str("\n\n");
        }
    }
    let labels: HashMap<i64, String> = detail
        .nodes
        .iter()
        .map(|node| {
            let label = match node.item_id {
                Some(item_id) => detail
                    .items
                    .iter()
                    .find(|item| item.id == item_id)
                    .map(|item| format!("#{} {}", item.id, card_label(item)))
                    .unwrap_or_else(|| "Card".into()),
                None => format!("Concept: {}", node.label),
            };
            (node.id, label)
        })
        .collect();
    let accepted: Vec<&EdgeView> = detail.edges.iter().filter(|edge| !edge.suggested).collect();
    if !accepted.is_empty() {
        out.push_str("## Connections\n\n");
        for edge in accepted {
            let from = labels.get(&edge.from).map_or("?", String::as_str);
            let to = labels.get(&edge.to).map_or("?", String::as_str);
            out.push_str(&format!("- {from} → {to}\n"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x * 7) as u8, (y * 5) as u8, 128])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn service() -> (tempfile::TempDir, InvestigationService) {
        let dir = tempfile::tempdir().unwrap();
        let service = InvestigationService::open(dir.path()).unwrap();
        (dir, service)
    }

    #[test]
    fn first_investigation_becomes_the_quick_capture_target() {
        let (_dir, service) = service();
        assert_eq!(service.active().unwrap(), None);
        let first = service.create("First").unwrap();
        let second = service.create("Second").unwrap();
        assert_eq!(service.active().unwrap(), Some(first));
        service.set_active(Some(second)).unwrap();
        service.delete(second).unwrap();
        assert_eq!(service.active().unwrap(), None);
        let created = service.active_or_create().unwrap();
        assert_eq!(service.title(created).unwrap(), "Quick captures");
        assert_eq!(service.active_or_create().unwrap(), created);
    }

    #[test]
    fn text_becomes_a_note_text_or_link_card() {
        let (_dir, service) = service();
        let id = service.create("Case").unwrap();
        let note = service
            .add_text(id, " remember this ", true, "typed", Attachment::default())
            .unwrap();
        let pasted = service
            .add_text(id, "pasted words", false, "paste", Attachment::default())
            .unwrap();
        let link = service
            .add_text(
                id,
                "https://example.com/a",
                false,
                "paste",
                Attachment::default(),
            )
            .unwrap();
        assert!(
            service
                .add_text(id, "   ", true, "typed", Attachment::default())
                .is_err()
        );
        let detail = service.detail(id, None).unwrap().unwrap();
        let kinds: Vec<_> = detail
            .items
            .iter()
            .map(|item| (item.id, item.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![(note, "note"), (pasted, "text"), (link, "link")]
        );
        assert_eq!(detail.items[0].body, "remember this");
        assert_eq!(detail.items[2].fetch_state, Some("pending"));
        assert_eq!(service.search(id, "PASTED", None).unwrap(), vec![pasted]);
        assert_eq!(service.search(id, "", Some("link")).unwrap(), vec![link]);
        assert!(service.search(id, "", Some("bogus")).is_err());
    }

    #[test]
    fn images_are_reencoded_by_hash_shared_and_deleted_with_their_last_card() {
        let (dir, service) = service();
        let id = service.create("Case").unwrap();
        let bytes = png(40, 30);
        let first = service
            .add_image(id, &bytes, "paste", Attachment::default())
            .unwrap();
        let encoded = format!("data:image/png;base64,{}", STANDARD.encode(&bytes));
        let second = service
            .add_image_base64(id, &encoded, "drop", Attachment::default())
            .unwrap();
        let detail = service.detail(id, None).unwrap().unwrap();
        let sha = detail.items[0].image_sha.clone().unwrap();
        assert_eq!(detail.items[1].image_sha.as_deref(), Some(sha.as_str()));
        assert_eq!(
            (detail.items[0].image_width, detail.items[0].image_height),
            (40, 30)
        );
        assert_eq!(detail.items[0].ocr_state, Some("pending"));
        let file = dir
            .path()
            .join("investigations")
            .join(id.to_string())
            .join(format!("{sha}.png"));
        assert!(file.is_file());
        assert!(
            service
                .image_data_url(id, &sha)
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(service.image_data_url(id, "../../secret").is_err());
        let other = service.create("Other").unwrap();
        assert!(service.image_data_url(other, &sha).is_err());
        service.delete_item(first).unwrap();
        assert!(file.is_file());
        service.delete_item(second).unwrap();
        assert!(!file.exists());
        assert!(
            service
                .add_image(id, b"not an image", "paste", Attachment::default())
                .is_err()
        );
    }

    #[test]
    fn deleting_an_investigation_removes_its_image_folder() {
        let (dir, service) = service();
        let id = service.create("Case").unwrap();
        service
            .add_image(id, &png(8, 8), "paste", Attachment::default())
            .unwrap();
        let folder = dir.path().join("investigations").join(id.to_string());
        assert!(folder.is_dir());
        service.delete(id).unwrap();
        assert!(!folder.exists());
        fs::create_dir_all(dir.path().join("investigations").join("999")).unwrap();
        service.remove_orphaned_folders();
        assert!(!dir.path().join("investigations").join("999").exists());
    }

    #[test]
    fn plus_button_captures_are_connected_and_placed_beside_their_parent() {
        let (_dir, service) = service();
        let id = service.create("Case").unwrap();
        let concept = service
            .add_concept(id, "Supplier", 100.0, 50.0, Attachment::default())
            .unwrap();
        let parent = Attachment {
            parent_node: Some(concept),
        };
        service
            .add_text(id, "first", true, "typed", parent)
            .unwrap();
        service
            .add_text(id, "second", true, "typed", parent)
            .unwrap();
        let detail = service.detail(id, None).unwrap().unwrap();
        assert_eq!(detail.edges.len(), 2);
        assert!(
            detail
                .edges
                .iter()
                .all(|edge| edge.from == concept && !edge.suggested)
        );
        let placed: Vec<_> = detail
            .nodes
            .iter()
            .filter(|node| node.item_id.is_some())
            .collect();
        assert_eq!((placed[0].x, placed[0].y), (400.0, 50.0));
        assert_eq!((placed[1].x, placed[1].y), (400.0, 200.0));
        let other = service.create("Other").unwrap();
        assert!(service.add_text(other, "x", true, "typed", parent).is_err());
        // Unplaced cards stay in the tray.
        let loose = service
            .add_text(id, "loose", true, "typed", Attachment::default())
            .unwrap();
        let detail = service.detail(id, None).unwrap().unwrap();
        assert_eq!(
            detail
                .items
                .iter()
                .find(|item| item.id == loose)
                .unwrap()
                .node_id,
            None
        );
    }

    #[test]
    fn detail_is_skipped_when_nothing_changed() {
        let (_dir, service) = service();
        let id = service.create("Case").unwrap();
        let detail = service.detail(id, None).unwrap().unwrap();
        assert!(service.detail(id, Some(detail.revision)).unwrap().is_none());
        service
            .add_text(id, "change", true, "typed", Attachment::default())
            .unwrap();
        assert!(service.detail(id, Some(detail.revision)).unwrap().is_some());
    }

    #[test]
    fn markdown_export_lists_cards_and_accepted_connections_only() {
        let (_dir, service) = service();
        let id = service.create("Supplier audit").unwrap();
        let concept = service
            .add_concept(id, "Pricing", 0.0, 0.0, Attachment::default())
            .unwrap();
        let note = service
            .add_text(
                id,
                "Quote was 21,950 USD\n\nSecond paragraph",
                true,
                "typed",
                Attachment {
                    parent_node: Some(concept),
                },
            )
            .unwrap();
        let other = service
            .add_text(id, "unrelated", true, "typed", Attachment::default())
            .unwrap();
        let other_node = service.place_item(other, 10.0, 10.0).unwrap();
        service
            .db()
            .unwrap()
            .investigations()
            .add_edge(concept, other_node, EdgeStatus::Suggested, "", "maybe", 1)
            .unwrap();
        let markdown = service.export_markdown(id).unwrap();
        assert!(markdown.starts_with("# Supplier audit\n"));
        assert!(markdown.contains(&format!("### #{note} · Note — Quote was 21,950 USD")));
        assert!(markdown.contains("> Quote was 21,950 USD\n>\n> Second paragraph"));
        assert!(markdown.contains(&format!(
            "- Concept: Pricing → #{note} Quote was 21,950 USD"
        )));
        assert!(!markdown.contains("unrelated\n- "));
        assert_eq!(markdown.matches(" → ").count(), 1);
    }

    #[test]
    fn quick_capture_files_text_links_and_clipboard_images_into_the_active_investigation() {
        let (_dir, service) = service();
        let outcome = service
            .quick_capture(ClipboardCapture::Text("https://example.com/x".into()))
            .unwrap();
        assert_eq!(outcome.investigation_title, "Quick captures");
        assert_eq!(outcome.kind, "link");
        let chosen = service.create("Chosen").unwrap();
        service.set_active(Some(chosen)).unwrap();
        let outcome = service
            .quick_capture(ClipboardCapture::Image {
                width: 2,
                height: 1,
                rgba: vec![255, 0, 0, 255, 0, 0, 255, 128],
            })
            .unwrap();
        assert_eq!((outcome.investigation_id, outcome.kind), (chosen, "image"));
        assert!(
            service
                .quick_capture(ClipboardCapture::Image {
                    width: 3,
                    height: 3,
                    rgba: vec![0; 4]
                })
                .is_err()
        );
        assert_eq!(
            service
                .quick_capture(ClipboardCapture::Text("plain words".into()))
                .unwrap()
                .kind,
            "text"
        );
        let items = service.detail(chosen, None).unwrap().unwrap().items;
        assert_eq!(
            items
                .iter()
                .map(|item| item.source.as_str())
                .collect::<Vec<_>>(),
            vec!["quick-capture"; 2]
        );
    }

    #[test]
    fn utc_dates_are_formatted_without_a_time_zone_database() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_time(1_792_209_600_000), "2026-10-17 04:00 UTC");
    }

    #[test]
    fn card_labels_prefer_titles_then_previews_then_text() {
        let mut item = ItemView {
            id: 1,
            kind: "link",
            title: String::new(),
            body: String::new(),
            url: Some("https://example.com".into()),
            link_title: None,
            link_description: None,
            link_site: None,
            image_sha: None,
            image_width: 0,
            image_height: 0,
            favicon_sha: None,
            ocr_state: None,
            fetch_state: None,
            source: "paste".into(),
            created_at_ms: 0,
            updated_at_ms: 0,
            node_id: None,
        };
        assert_eq!(card_label(&item), "https://example.com");
        item.link_title = Some("Example Domain".into());
        assert_eq!(card_label(&item), "Example Domain");
        item.title = "  My   title ".into();
        assert_eq!(card_label(&item), "My title");
        item.kind = "image";
        item.title.clear();
        item.link_title = None;
        item.url = None;
        assert_eq!(card_label(&item), "Screenshot");
        item.body = format!("\n{}", "x".repeat(100));
        assert_eq!(card_label(&item).chars().count(), 80);
    }
}
