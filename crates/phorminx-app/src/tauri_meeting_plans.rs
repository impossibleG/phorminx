//! Tauri command boundary for scheduled meetings. A plan keeps the title,
//! time, instructions, important items and briefing; its materials (text,
//! documents, images, voice notes and the prep chat) live in a linked
//! investigation so they reuse OCR, voice capture and the assistant.

use std::thread;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use phorminx_assistant::{ChatMessage, ChatRole};
use phorminx_persistence::{MeetingPlanDraft, MeetingPlanRecord};
use phorminx_ui::workspace::WorkspaceEvent;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::investigation_ocr::now_ms;
use crate::investigations::{Attachment, InvestigationService, card_label};
use crate::meeting_documents::{self, DocumentKind, MAX_DOCUMENT_BYTES};
use crate::meeting_prep::{self, PlanContext};
use crate::tauri_contract::ImportantItemView;
use crate::tauri_investigations::{InvestigationTauriState, show_toast_for};
use crate::tauri_meetings::{MeetingTauriState, open_companion};

const UPCOMING_LIMIT: usize = 50;
const REMINDER_CHECK: Duration = Duration::from_secs(30);
const REMINDER_VISIBLE: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingPlanView {
    pub id: i64,
    pub title: String,
    pub scheduled_at_ms: Option<i64>,
    pub reminder_minutes: Option<u32>,
    pub instructions: String,
    pub briefing: String,
    pub investigation_id: Option<i64>,
    pub session_id: Option<i64>,
    pub items: Vec<ImportantItemView>,
}

impl From<MeetingPlanRecord> for MeetingPlanView {
    fn from(plan: MeetingPlanRecord) -> Self {
        Self {
            id: plan.id,
            title: plan.title,
            scheduled_at_ms: plan.scheduled_at_ms,
            reminder_minutes: plan.reminder_minutes,
            instructions: plan.instructions,
            briefing: plan.briefing,
            investigation_id: plan.investigation_id,
            session_id: plan.session_id,
            items: plan
                .items
                .into_iter()
                .map(|item| ImportantItemView {
                    text: item.text,
                    done: item.done,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeetingPlanInput {
    pub title: String,
    pub scheduled_at_ms: Option<i64>,
    pub reminder_minutes: Option<u32>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub briefing: String,
    #[serde(default)]
    pub items: Vec<ImportantItemView>,
}

impl MeetingPlanInput {
    fn draft(self) -> MeetingPlanDraft {
        MeetingPlanDraft {
            title: self.title,
            scheduled_at_ms: self.scheduled_at_ms,
            reminder_minutes: self.reminder_minutes,
            instructions: self.instructions,
            briefing: self.briefing,
            items: self
                .items
                .into_iter()
                .map(|item| (item.text.trim().to_owned(), item.done))
                .filter(|(text, _)| !text.is_empty())
                .collect(),
        }
    }
}

fn plan(service: &InvestigationService, id: i64) -> Result<MeetingPlanRecord, String> {
    service
        .db()?
        .meeting_plans()
        .get(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "This meeting plan no longer exists.".to_owned())
}

/// True while a meeting is being recorded or finished.
fn recording(meeting: &MeetingTauriState) -> Result<(bool, Option<i64>), String> {
    let view = meeting
        .snapshot(u64::MAX)?
        .ok_or("Meeting state is temporarily busy. Please retry.")?;
    Ok((view.capture != "idle", view.plan_id))
}

/// Asks a meeting showing this plan to reload it, so edits reach the
/// system prompt without restarting.
fn reload(meeting: &MeetingTauriState) {
    let _ = meeting.send(WorkspaceEvent::ReloadMeetingPlan);
}

#[tauri::command]
pub(crate) fn meeting_plans(
    state: State<'_, InvestigationTauriState>,
) -> Result<Vec<MeetingPlanView>, String> {
    Ok(state
        .service()?
        .db()?
        .meeting_plans()
        .upcoming(UPCOMING_LIMIT)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(MeetingPlanView::from)
        .collect())
}

#[tauri::command]
pub(crate) fn meeting_plan(
    id: i64,
    state: State<'_, InvestigationTauriState>,
) -> Result<MeetingPlanView, String> {
    plan(state.service()?, id).map(MeetingPlanView::from)
}

#[tauri::command]
pub(crate) fn meeting_plan_save(
    id: Option<i64>,
    plan: MeetingPlanInput,
    state: State<'_, InvestigationTauriState>,
    meeting: State<'_, MeetingTauriState>,
) -> Result<i64, String> {
    let draft = plan.draft();
    if draft.title.trim().is_empty() {
        return Err("Give the meeting a title.".into());
    }
    let service = state.service()?;
    let id = {
        let db = service.db()?;
        let plans = db.meeting_plans();
        match id {
            Some(id) => {
                if !plans
                    .update(id, &draft, now_ms())
                    .map_err(|error| error.to_string())?
                {
                    return Err("This meeting plan no longer exists.".into());
                }
                id
            }
            None => plans
                .create(&draft, now_ms())
                .map_err(|error| error.to_string())?,
        }
    };
    if let Ok(record) = self::plan(service, id)
        && let Some(investigation) = record.investigation_id
    {
        let _ = service.rename(investigation, &materials_title(&record.title));
    }
    reload(&meeting);
    Ok(id)
}

#[tauri::command]
pub(crate) fn meeting_plan_delete(
    id: i64,
    state: State<'_, InvestigationTauriState>,
    meeting: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    if recording(&meeting)? == (true, Some(id)) {
        return Err("Finish this meeting before deleting its plan.".into());
    }
    let service = state.service()?;
    let record = plan(service, id)?;
    if let Some(investigation) = record.investigation_id {
        if let Ok(assistant) = state.assistant() {
            assistant.cancel_for(investigation);
        }
        let _ = service.delete(investigation);
    }
    service
        .db()?
        .meeting_plans()
        .delete(id)
        .map_err(|error| error.to_string())?;
    reload(&meeting);
    Ok(())
}

fn materials_title(title: &str) -> String {
    format!("Meeting prep: {}", title.trim())
        .chars()
        .take(200)
        .collect()
}

/// The investigation holding this plan's materials, created on first use.
fn ensure_materials(service: &InvestigationService, id: i64) -> Result<i64, String> {
    let record = plan(service, id)?;
    if let Some(investigation) = record.investigation_id
        && service.title(investigation).is_ok()
    {
        return Ok(investigation);
    }
    let investigation = service.create(&materials_title(&record.title))?;
    service
        .db()?
        .meeting_plans()
        .link_investigation(id, investigation)
        .map_err(|error| error.to_string())?;
    Ok(investigation)
}

#[tauri::command]
pub(crate) fn meeting_plan_materials(
    id: i64,
    state: State<'_, InvestigationTauriState>,
    meeting: State<'_, MeetingTauriState>,
) -> Result<i64, String> {
    let investigation = ensure_materials(state.service()?, id)?;
    reload(&meeting);
    Ok(investigation)
}

/// Adds a PDF, Word, text or image file. Text is split into cards so the
/// meeting looks up only the parts that match what is being discussed.
#[tauri::command]
pub(crate) async fn meeting_plan_add_document(
    id: i64,
    name: String,
    data: String,
    state: State<'_, InvestigationTauriState>,
    meeting: State<'_, MeetingTauriState>,
) -> Result<usize, String> {
    let kind =
        meeting_documents::kind_of(&name).ok_or("Add a PDF, Word (.docx), text or image file.")?;
    let service = state.service()?.clone();
    let investigation = ensure_materials(&service, id)?;
    reload(&meeting);
    let name: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect();
    tauri::async_runtime::spawn_blocking(move || {
        let attachment = || Attachment { parent_node: None };
        if kind == DocumentKind::Image {
            service.add_image_base64(investigation, &data, "file", attachment())?;
            return Ok(1);
        }
        let payload = data.split_once(',').map_or(data.as_str(), |(_, rest)| rest);
        if payload.len() > MAX_DOCUMENT_BYTES / 3 * 4 + 8 {
            return Err("Documents up to 25 MB can be added.".to_owned());
        }
        let bytes = STANDARD
            .decode(payload.trim())
            .map_err(|_| "The file could not be read.")?;
        let text = meeting_documents::extract_text(&kind, &bytes)?;
        let parts = meeting_documents::split_parts(&text);
        let count = parts.len();
        for (index, part) in parts.into_iter().enumerate() {
            let item = service.add_text(investigation, &part, false, "file", attachment())?;
            let title = if count == 1 {
                name.clone()
            } else {
                format!("{name} (part {} of {count})", index + 1)
            };
            service.update_item(item, &title, &part)?;
        }
        Ok(count)
    })
    .await
    .map_err(|_| "The file could not be added.".to_owned())?
}

/// The plan's context plus everything gathered for it, condensed.
fn digest(service: &InvestigationService, id: i64) -> Result<(PlanContext, String), String> {
    let context = PlanContext::from(plan(service, id)?);
    let (cards, chat) = match context.investigation_id {
        Some(investigation) => match service.detail(investigation, None)? {
            Some(detail) => (
                detail
                    .items
                    .iter()
                    .filter(|item| !item.body.trim().is_empty())
                    .map(|item| (card_label(item), item.body.clone()))
                    .collect(),
                detail
                    .messages
                    .iter()
                    .filter(|message| message.status == "complete")
                    .map(|message| (message.role.clone(), message.text.clone()))
                    .collect(),
            ),
            None => (Vec::new(), Vec::new()),
        },
        None => (Vec::new(), Vec::new()),
    };
    let digest = meeting_prep::prep_digest(&context, &cards, &chat);
    Ok((context, digest))
}

async fn complete(
    state: &InvestigationTauriState,
    id: i64,
    instructions: &'static str,
) -> Result<(PlanContext, String), String> {
    let service = state.service()?.clone();
    let assistant = state.assistant()?.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (context, digest) = digest(&service, id)?;
        let reply = assistant.complete(&[
            ChatMessage {
                role: ChatRole::System,
                content: instructions.into(),
            },
            ChatMessage {
                role: ChatRole::User,
                content: digest,
            },
        ])?;
        Ok((context, reply.trim().to_owned()))
    })
    .await
    .map_err(|_| "The assistant could not answer.".to_owned())?
}

/// Writes (or rewrites) the briefing from the plan's materials and saves it.
#[tauri::command]
pub(crate) async fn meeting_plan_briefing(
    id: i64,
    state: State<'_, InvestigationTauriState>,
    meeting: State<'_, MeetingTauriState>,
) -> Result<String, String> {
    let (_, briefing) = complete(&state, id, meeting_prep::BRIEFING_PROMPT).await?;
    if briefing.is_empty() {
        return Err("The assistant returned an empty briefing.".into());
    }
    let briefing: String = briefing
        .chars()
        .take(phorminx_persistence::MAX_PLAN_TEXT_BYTES / 4)
        .collect();
    state
        .service()?
        .db()?
        .meeting_plans()
        .set_briefing(id, &briefing, now_ms())
        .map_err(|error| error.to_string())?;
    reload(&meeting);
    Ok(briefing)
}

/// Suggested important items; the user picks which to keep.
#[tauri::command]
pub(crate) async fn meeting_plan_suggest_items(
    id: i64,
    state: State<'_, InvestigationTauriState>,
) -> Result<Vec<String>, String> {
    let (context, reply) = complete(&state, id, meeting_prep::SUGGEST_ITEMS_PROMPT).await?;
    let items = meeting_prep::parse_suggested_items(&reply, &context.items);
    if items.is_empty() {
        return Err("The assistant had no new items to suggest.".into());
    }
    Ok(items)
}

/// Starts recording with this plan's context and opens the companion.
#[tauri::command]
pub(crate) fn meeting_plan_start(
    id: i64,
    app: tauri::AppHandle,
    state: State<'_, InvestigationTauriState>,
    meeting: State<'_, MeetingTauriState>,
) -> Result<(), String> {
    if recording(&meeting)?.0 {
        return Err("Finish the current meeting first.".into());
    }
    plan(state.service()?, id)?;
    meeting.send(WorkspaceEvent::New)?;
    meeting.send(WorkspaceEvent::SetMeetingPlan(Some(id)))?;
    meeting.send(WorkspaceEvent::StartMeeting)?;
    open_companion(&app)
}

/// Checks for due reminders and shows a toast for each.
pub fn spawn_reminders(app: tauri::AppHandle) {
    let _ = thread::Builder::new()
        .name("phorminx-meeting-reminders".into())
        .spawn(move || {
            loop {
                thread::sleep(REMINDER_CHECK);
                let state = app.state::<InvestigationTauriState>();
                let Ok(service) = state.service() else {
                    return;
                };
                let now = now_ms();
                let due = service
                    .db()
                    .ok()
                    .and_then(|db| db.meeting_plans().due_reminders(now).ok())
                    .unwrap_or_default();
                for plan in due {
                    if let Ok(db) = service.db() {
                        let _ = db.meeting_plans().mark_reminded(plan.id);
                    }
                    let detail = match plan.scheduled_at_ms.map(|at| (at - now) / 60_000) {
                        Some(minutes) if minutes >= 1 => format!(
                            "Starts in {minutes} minute{}. Open Meetings › Upcoming to start it with your prep.",
                            if minutes == 1 { "" } else { "s" }
                        ),
                        Some(minutes) if minutes <= -2 => format!(
                            "Started {} minutes ago. Open Meetings › Upcoming to start it with your prep.",
                            -minutes
                        ),
                        _ => "Starting now. Open Meetings › Upcoming to start it with your prep."
                            .to_owned(),
                    };
                    show_toast_for(
                        &app,
                        &state,
                        &format!("Meeting soon: {}", plan.title),
                        &detail,
                        false,
                        REMINDER_VISIBLE,
                    );
                }
            }
        });
}
