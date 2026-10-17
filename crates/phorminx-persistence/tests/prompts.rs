use phorminx_persistence::{Persistence, PersistenceError, PromptDraft};

const NOW: i64 = 2_000_000_000_000;

fn open_temp() -> (tempfile::TempDir, Persistence) {
    let directory = tempfile::tempdir().unwrap();
    let database = Persistence::open(directory.path().join("phorminx.sqlite3")).unwrap();
    (directory, database)
}

fn draft(name: &str, tags: &[&str], body: &str) -> PromptDraft {
    PromptDraft {
        name: name.to_owned(),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        body: body.to_owned(),
    }
}

#[test]
fn every_save_is_a_version_and_restore_appends_one() {
    let (_directory, database) = open_temp();
    let prompts = database.prompts();
    let id = prompts
        .create(
            &draft(
                "  Meeting   notes ",
                &["#Work", "work", " notes "],
                " Use bullets. ",
            ),
            NOW,
        )
        .unwrap();
    let created = prompts.get(id).unwrap().unwrap();
    assert_eq!(created.name, "Meeting notes");
    assert_eq!(created.tags, ["work", "notes"]);
    assert_eq!(created.body, "Use bullets.");
    assert_eq!(created.version, 1);

    assert!(
        prompts
            .save(
                id,
                &draft("Meeting notes", &["work"], "Use numbered steps."),
                NOW + 1
            )
            .unwrap()
    );
    assert_eq!(
        prompts.get(id).unwrap().unwrap().body,
        "Use numbered steps."
    );
    let versions = prompts.versions(id).unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|version| version.version)
            .collect::<Vec<_>>(),
        [2, 1]
    );

    assert!(prompts.restore(id, 1, NOW + 2).unwrap());
    let restored = prompts.get(id).unwrap().unwrap();
    assert_eq!(restored.body, "Use bullets.");
    assert_eq!(restored.version, 3);
    assert_eq!(restored.tags, ["work"], "restore keeps the current tags");
    assert_eq!(prompts.versions(id).unwrap().len(), 3);
    assert!(!prompts.restore(id, 99, NOW + 3).unwrap());
}

#[test]
fn names_are_unique_and_inputs_are_validated() {
    let (_directory, database) = open_temp();
    let prompts = database.prompts();
    prompts
        .create(&draft("Email", &[], "Write an email."), NOW)
        .unwrap();
    assert!(matches!(
        prompts.create(&draft("email", &[], "Another."), NOW),
        Err(PersistenceError::Validation {
            field: "prompt_name",
            ..
        })
    ));
    assert!(matches!(
        prompts.create(&draft(" ", &[], "Body"), NOW),
        Err(PersistenceError::Validation {
            field: "prompt_name",
            ..
        })
    ));
    assert!(matches!(
        prompts.create(&draft("Empty", &[], "  "), NOW),
        Err(PersistenceError::Validation {
            field: "prompt_body",
            ..
        })
    ));
    assert!(matches!(
        prompts.create(&draft("Long", &[], &"x".repeat(5_000)), NOW),
        Err(PersistenceError::Validation {
            field: "prompt_body",
            ..
        })
    ));
}

#[test]
fn delete_removes_the_prompt_and_its_history_and_lists_stay_sorted() {
    let (_directory, database) = open_temp();
    let prompts = database.prompts();
    let zebra = prompts.create(&draft("zebra", &[], "Z"), NOW).unwrap();
    let alpha = prompts.create(&draft("Alpha", &[], "A"), NOW).unwrap();
    prompts
        .save(alpha, &draft("Alpha", &[], "A2"), NOW + 1)
        .unwrap();
    let names = prompts
        .list()
        .unwrap()
        .into_iter()
        .map(|prompt| (prompt.name, prompt.body))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [("Alpha".into(), "A2".into()), ("zebra".into(), "Z".into())]
    );

    assert!(prompts.delete(alpha).unwrap());
    assert!(prompts.get(alpha).unwrap().is_none());
    assert!(prompts.versions(alpha).unwrap().is_empty());
    assert!(
        !prompts
            .save(alpha, &draft("Alpha", &[], "back"), NOW)
            .unwrap()
    );
    assert!(!prompts.delete(alpha).unwrap());
    assert_eq!(prompts.list().unwrap().len(), 1);
    assert!(prompts.get(zebra).unwrap().is_some());
}

#[test]
fn prompts_survive_reopening_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("phorminx.sqlite3");
    let id = Persistence::open(&path)
        .unwrap()
        .prompts()
        .create(&draft("Kept", &["a"], "Body"), NOW)
        .unwrap();
    let reopened = Persistence::open(&path).unwrap();
    assert_eq!(reopened.prompts().get(id).unwrap().unwrap().name, "Kept");
}
