//! Measures accuracy and latency on screenshot fixtures:
//! `cargo run --release -p phorminx-ocr --example ocr_spike -- <det.onnx> <rec.onnx> <dict.txt> <dir>`
//! Each `<name>.png` in `<dir>` is compared with the expected text in `<name>.txt`.

use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: ocr_spike <det.onnx> <rec.onnx> <dict.txt> <fixtures-dir>");
        std::process::exit(2);
    }
    let started = Instant::now();
    let engine = phorminx_ocr::OcrEngine::load(
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
    )
    .expect("models load");
    println!("load_ms={}", started.elapsed().as_millis());
    let mut entries: Vec<_> = std::fs::read_dir(&args[4])
        .expect("fixture dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect();
    entries.sort();
    let (mut total_edits, mut total_chars) = (0usize, 0usize);
    for path in entries {
        let bytes = std::fs::read(&path).expect("fixture");
        let expected = std::fs::read_to_string(path.with_extension("txt")).unwrap_or_default();
        let image = phorminx_ocr::decode_image(&bytes).expect("decode");
        let started = Instant::now();
        let lines = engine.recognize(&image).expect("recognize");
        let elapsed = started.elapsed().as_millis();
        let actual = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let edits = edit_distance(&normalize(&expected), &normalize(&actual)).min(edit_distance(
            &sorted_lines(&expected),
            &sorted_lines(&actual),
        ));
        let chars = normalize(&expected).chars().count();
        total_edits += edits;
        total_chars += chars;
        println!(
            "{} {}x{} lines={} ms={} cer={:.2}%",
            path.file_stem().unwrap().to_string_lossy(),
            image.width(),
            image.height(),
            lines.len(),
            elapsed,
            100.0 * edits as f64 / chars.max(1) as f64
        );
        for line in &lines {
            println!("    [{:.2}] {}", line.confidence, line.text);
        }
    }
    println!(
        "overall_cer={:.2}%",
        100.0 * total_edits as f64 / total_chars.max(1) as f64
    );
}

/// Reading order is reported separately; this compares the line multiset.
fn sorted_lines(text: &str) -> String {
    let mut lines: Vec<String> = text
        .lines()
        .map(normalize)
        .filter(|l| !l.is_empty())
        .collect();
    lines.sort();
    lines.join(" ")
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(ca != cb))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        previous = current;
    }
    previous[b.len()]
}
