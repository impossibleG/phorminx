//! Seeds a throwaway profile with synthetic content for README screenshots.
//!
//! Usage: `cargo run -p phorminx-persistence --example readme_demo_profile -- <empty-dir>`
//!
//! Every name, number and sentence below is made up. The directory must be
//! new or empty; this never opens an existing profile. See
//! docs/features/CAPTURE.md for the settings files that go beside it.
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use phorminx_persistence::{
    AppProfile, CasePolicy, DictationDraft, EdgeStatus, ExecutableIdentity, FormattingStyle,
    InsertionPreference, InvestigationItemKind, InvestigationMessageStatus, LinkPreviewUpdate,
    MeetingMessageStatus, NewInvestigationItem, NewLexiconEntry, Persistence, PromptDraft,
    RetentionPolicy, TimingMetadata,
};

const MINUTE: i64 = 60_000;
const SAMPLE_RATE: u64 = 16_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("pass an empty directory for the demo profile")?,
    );
    if directory.exists() && std::fs::read_dir(&directory)?.next().is_some() {
        return Err("the demo profile directory must be empty".into());
    }
    std::fs::create_dir_all(&directory)?;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let db = Persistence::open(directory.join("phorminx.db"))?;
    db.history()
        .set_retention(RetentionPolicy::Indefinite, now)?;

    seed_dictations(&db, now)?;
    seed_meeting(&db, now)?;
    seed_prompts(&db, now)?;
    seed_investigation(&db, now)?;
    seed_lexicon_and_profiles(&db)?;
    println!("Seeded {}", directory.display());
    Ok(())
}

fn seed_dictations(db: &Persistence, now: i64) -> Result<(), Box<dyn std::error::Error>> {
    let dictations = [
        (
            "notepad.exe",
            95,
            "ok so for the launch checklist we need the new icons the release notes and uh the installer smoke test on a clean machine",
            "For the launch checklist we need:\n\n- The new icons\n- The release notes\n- The installer smoke test on a clean machine",
        ),
        (
            "outlook.exe",
            62,
            "hi priya thanks for the notes from tuesday i moved the design review to thursday at ten does that still work for you",
            "Hi Priya,\n\nThanks for the notes from Tuesday. I moved the design review to Thursday at 10:00. Does that still work for you?",
        ),
        (
            "code.exe",
            31,
            "add a comment saying the retry budget is three attempts with exponential backoff starting at two hundred milliseconds",
            "// The retry budget is three attempts with exponential backoff starting at 200 ms.",
        ),
        (
            "slack.exe",
            12,
            "running about five minutes late grabbing coffee for the team want anything",
            "Running about five minutes late. Grabbing coffee for the team. Want anything?",
        ),
        (
            "winword.exe",
            4,
            "the pilot ran for six weeks with forty two participants and average setup time dropped from eleven minutes to under four",
            "The pilot ran for six weeks with 42 participants, and average setup time dropped from 11 minutes to under 4.",
        ),
    ];
    for (target, minutes_ago, raw, formatted) in dictations {
        db.history().insert(&DictationDraft {
            created_at_ms: now - minutes_ago * MINUTE,
            raw_text: raw.into(),
            normalized_text: None,
            cleaned_text: Some(formatted.into()),
            selected_output: formatted.into(),
            language: Some("en".into()),
            target_executable: Some(target.into()),
            timings: TimingMetadata {
                audio_duration_ms: Some(raw.len() as u64 * 55),
                stt_duration_ms: Some(240),
                formatting_duration_ms: Some(610),
                ..TimingMetadata::default()
            },
            warnings: Vec::new(),
        })?;
    }
    Ok(())
}

fn seed_meeting(db: &Persistence, now: i64) -> Result<(), Box<dyn std::error::Error>> {
    let started = now - 40 * MINUTE;
    let meetings = db.meetings();
    let id = meetings
        .create("Weekly product sync", "system", started)?
        .ok_or("history retention is disabled")?;
    meetings.mark_dual_source(id)?;
    let lines = [
        (
            "system",
            "Morning everyone. Let's start with the beta numbers from last week.",
        ),
        (
            "mic",
            "Sure. We had three hundred and twelve new installs and setup finished for ninety one percent of them.",
        ),
        (
            "system",
            "That's better than the previous build. What happened with the other nine percent?",
        ),
        (
            "mic",
            "Mostly slow model downloads. Two people closed the window before the speech model finished.",
        ),
        (
            "system",
            "Can we show the remaining time more clearly during that step?",
        ),
        (
            "mic",
            "Yes, I'll add an estimate next to the progress bar and keep the stop button visible.",
        ),
        (
            "system",
            "Great. Next topic is the Lisbon workshop on the twenty fourth.",
        ),
        (
            "system",
            "We still need someone to run the live translation demo for the Portuguese speakers.",
        ),
    ];
    let translations = [
        "Bom dia a todos. Vamos começar com os números do beta da semana passada.",
        "Claro. Tivemos trezentas e doze novas instalações e a configuração terminou para noventa e um por cento delas.",
        "Isso é melhor do que a versão anterior. O que aconteceu com os outros nove por cento?",
        "Principalmente downloads lentos de modelos. Duas pessoas fecharam a janela antes de o modelo de fala terminar.",
        "Podemos mostrar o tempo restante com mais clareza nessa etapa?",
        "Sim, vou adicionar uma estimativa ao lado da barra de progresso e manter o botão de parar visível.",
        "Ótimo. O próximo tema é o workshop de Lisboa no dia vinte e quatro.",
        "Ainda precisamos de alguém para conduzir a demonstração de tradução ao vivo para quem fala português.",
    ];
    let mut start = 0u64;
    for (sequence, ((source, text), translated)) in lines.iter().zip(translations).enumerate() {
        let end = start + (text.len() as u64 / 14 + 2) * SAMPLE_RATE;
        let sequence = sequence as u64;
        let at = started + (end / SAMPLE_RATE) as i64 * 1000;
        meetings.append_segment(id, sequence, start, end, text, at)?;
        meetings.save_source_segment(id, source, start, end, start, text)?;
        meetings.save_translation(
            id,
            sequence,
            "Portuguese (Brazil)",
            "qwen2.5:1.5b",
            translated,
        )?;
        start = end;
    }
    let asked = started + (start / SAMPLE_RATE) as i64 * 1000 + 5_000;
    meetings.append_user_message(
        id,
        start,
        "What did we decide about the setup step, and what is still open?",
        asked,
    )?;
    meetings.start_assistant(id, 1, asked)?;
    meetings.update_assistant(
        id,
        1,
        "**Decided**\n- Show a time estimate next to the model download progress bar.\n- Keep the stop button visible during that step.\n\n**Still open**\n- Who runs the live translation demo at the Lisbon workshop on the 24th.\n\nSuggested reply: \"I can run the translation demo if someone covers the Q&A.\"",
        MeetingMessageStatus::Complete,
        asked + 4_000,
    )?;
    meetings.finalize(id, asked + 60_000)?;
    Ok(())
}

fn seed_prompts(db: &Persistence, now: i64) -> Result<(), Box<dyn std::error::Error>> {
    let prompts = [
        (
            "Short email",
            &["email", "work"][..],
            "Write it as a short, friendly email. Greeting, two or three sentences, and a clear ask. No sign-off.",
        ),
        (
            "Meeting notes",
            &["meetings", "summary"][..],
            "Turn this into meeting notes with three headings: Decisions, Action items (owner and date), Open questions.",
        ),
        (
            "Code comment",
            &["code"][..],
            "Rewrite as a concise code comment. Present tense, no filler, keep identifiers exactly as spoken.",
        ),
        (
            "Portuguese reply",
            &["translation", "pt-BR"][..],
            "Reply in Brazilian Portuguese with a warm, professional tone. Keep names and numbers unchanged.",
        ),
    ];
    for (index, (name, tags, body)) in prompts.into_iter().enumerate() {
        db.prompts().create(
            &PromptDraft {
                name: name.into(),
                tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                body: body.into(),
            },
            now - (10 - index as i64) * MINUTE,
        )?;
    }
    Ok(())
}

fn seed_investigation(db: &Persistence, now: i64) -> Result<(), Box<dyn std::error::Error>> {
    let repo = db.investigations();
    let id = repo.create(
        "Why did onboarding drop in March?",
        now - 3 * 24 * 60 * MINUTE,
    )?;
    let mut at = now - 2 * 24 * 60 * MINUTE;
    let mut card = |kind, title: &str, body: &str, url: Option<&str>, source: &str| {
        at += 47 * MINUTE;
        let item = NewInvestigationItem {
            kind,
            title: title.into(),
            body: body.into(),
            url: url.map(Into::into),
            image: None,
            source: source.into(),
        };
        repo.add_item(id, &item, at).map(|item| (item, at))
    };
    let (metrics, _) = card(
        InvestigationItemKind::Text,
        "Funnel export",
        "Setup completion: Jan 93%, Feb 92%, Mar 81%, Apr 90%. Median setup time in March: 14 min (Feb: 6 min).",
        None,
        "clipboard",
    )?;
    let (release, _) = card(
        InvestigationItemKind::Note,
        "March release",
        "0.1.8 shipped on March 3 with the larger default speech model (466 MB instead of 142 MB).",
        None,
        "typed",
    )?;
    let (voice, _) = card(
        InvestigationItemKind::Voice,
        "Support call recap",
        "Three users said the setup window looked frozen while the model downloaded. One closed it after ten minutes.",
        None,
        "voice",
    )?;
    let (link, link_at) = card(
        InvestigationItemKind::Link,
        "",
        "Progress indicators reduce perceived wait time.",
        Some("https://example.com/research/progress-indicators"),
        "clipboard",
    )?;
    repo.save_link_preview(
        link,
        &LinkPreviewUpdate {
            title: Some("Designing progress indicators for long waits".into()),
            description: Some("Notes on showing remaining time, steps and cancel controls.".into()),
            site: Some("example.com".into()),
            image: None,
            favicon_sha256: None,
        },
        link_at,
    )?;
    let (fix, _) = card(
        InvestigationItemKind::Note,
        "April fix",
        "0.1.9 restored the small default model and added a download estimate.",
        None,
        "typed",
    )?;
    let (answer, _) = card(
        InvestigationItemKind::Answer,
        "Pinned answer",
        "Completion fell when 0.1.8 made the default model three times larger [#2], so setup took more than twice as long [#1] and looked frozen [#3]. It recovered after 0.1.9 [#5].",
        None,
        "assistant",
    )?;

    let place = |item, x, y| repo.place_item(item, x, y, now);
    let n_metrics = place(metrics, 80.0, 80.0)?;
    let n_release = place(release, 420.0, 40.0)?;
    let n_voice = place(voice, 420.0, 260.0)?;
    let n_link = place(link, 760.0, 300.0)?;
    let n_fix = place(fix, 760.0, 60.0)?;
    let _ = place(answer, 80.0, 330.0)?;
    let cause = repo.add_concept(id, "Bigger default model", 260.0, 170.0, now)?;
    repo.add_edge(
        n_release,
        cause,
        EdgeStatus::Accepted,
        "introduced",
        "",
        now,
    )?;
    repo.add_edge(
        cause,
        n_metrics,
        EdgeStatus::Accepted,
        "slowed setup",
        "",
        now,
    )?;
    repo.add_edge(
        cause,
        n_voice,
        EdgeStatus::Accepted,
        "looked frozen",
        "",
        now,
    )?;
    repo.add_edge(n_fix, n_release, EdgeStatus::Accepted, "reverted", "", now)?;
    repo.add_edge(
        n_link,
        n_voice,
        EdgeStatus::Suggested,
        "explains",
        "The article describes the same frozen-looking wait the users reported.",
        now,
    )?;

    repo.append_message(
        id,
        "user",
        InvestigationMessageStatus::Complete,
        "What caused the March drop?",
        now - 30 * MINUTE,
    )?;
    repo.append_message(
        id,
        "assistant",
        InvestigationMessageStatus::Complete,
        "The drop lines up with the 0.1.8 release, which tripled the default model size [#2]. Median setup time rose from 6 to 14 minutes [#1], and users described the window as frozen during the download [#3]. Completion returned to 90% after 0.1.9 restored the small model [#5].",
        now - 29 * MINUTE,
    )?;
    repo.set_active(Some(id))?;
    repo.set_capture_shortcut("Ctrl+Alt+Shift+F10")?;
    Ok(())
}

fn seed_lexicon_and_profiles(db: &Persistence) -> Result<(), Box<dyn std::error::Error>> {
    for (canonical, alias) in [
        ("Phorminx", "for mix"),
        ("Kubernetes", "cube or net ease"),
        ("Priya", "pria"),
    ] {
        db.lexicon().insert(&NewLexiconEntry {
            canonical: canonical.into(),
            alias: alias.into(),
            language: Some("en".into()),
            app_executable: None,
            case_policy: CasePolicy::UseCanonical,
            enabled: true,
        })?;
    }
    for (executable, style, instructions) in [
        (
            "outlook.exe",
            FormattingStyle::Custom,
            Some("@[Short email]"),
        ),
        ("code.exe", FormattingStyle::Custom, Some("@[Code comment]")),
        ("slack.exe", FormattingStyle::Light, None),
    ] {
        db.app_profiles().upsert(&AppProfile {
            executable: ExecutableIdentity::new(executable)?,
            formatting_style: style,
            custom_instructions: instructions.map(Into::into),
            language: None,
            insertion_preference: InsertionPreference::Automatic,
            deny: false,
        })?;
    }
    Ok(())
}
