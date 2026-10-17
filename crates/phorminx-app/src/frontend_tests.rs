//! Checks on the shipped web pages that the Rust suite can catch early.
use std::collections::HashSet;
use std::path::Path;

/// Scripts look elements up by id, so a duplicate silently hands one page's
/// element to another page's renderer (the meeting chat once wiped the whole
/// Messages page this way).
#[test]
fn shipped_pages_have_unique_element_ids() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/tauri-preview");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let html = std::fs::read_to_string(&path).unwrap();
        let mut seen = HashSet::new();
        for part in html.split(" id=\"").skip(1) {
            let id = part.split('"').next().unwrap();
            assert!(
                seen.insert(id.to_owned()),
                "duplicate id {id:?} in {}",
                path.display()
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected the shipped pages in {}",
        dir.display()
    );
}
