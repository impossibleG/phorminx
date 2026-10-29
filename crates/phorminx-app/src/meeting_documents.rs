//! Text from files added to a meeting's preparation: PDF, Word (.docx) and
//! plain text. Images are not handled here; they go to screenshot OCR.
use std::io::{Cursor, Read};

pub const MAX_DOCUMENT_BYTES: usize = 25 * 1024 * 1024;
/// Text is split into cards of about this size so lookups stay focused.
pub const PART_CHARS: usize = 4_000;
const MAX_PARTS: usize = 60;

#[derive(Debug, PartialEq, Eq)]
pub enum DocumentKind {
    Pdf,
    Word,
    Text,
    Image,
}

pub fn kind_of(name: &str) -> Option<DocumentKind> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "pdf" => DocumentKind::Pdf,
        "docx" => DocumentKind::Word,
        "txt" | "md" | "markdown" | "csv" | "json" | "log" => DocumentKind::Text,
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" => DocumentKind::Image,
        _ => return None,
    })
}

pub fn extract_text(kind: &DocumentKind, bytes: &[u8]) -> Result<String, String> {
    let text = match kind {
        DocumentKind::Pdf => pdf_extract::extract_text_from_mem(bytes).map_err(|_| {
            "This PDF could not be read. If it is a scan, add its pages as images instead."
                .to_owned()
        })?,
        DocumentKind::Word => docx_text(bytes)?,
        DocumentKind::Text => String::from_utf8_lossy(bytes).into_owned(),
        DocumentKind::Image => {
            return Err("Images are read with screenshot text recognition.".into());
        }
    };
    let text = tidy(&text);
    if text.is_empty() {
        return Err(match kind {
            DocumentKind::Pdf => {
                "This PDF has no selectable text. If it is a scan, add its pages as images instead."
            }
            _ => "This file has no text.",
        }
        .into());
    }
    Ok(text)
}

/// Splits on paragraph breaks into parts of at most `PART_CHARS`.
pub fn split_parts(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for paragraph in text.split("\n\n") {
        let mut paragraph = paragraph.trim().to_owned();
        while paragraph.chars().count() > PART_CHARS {
            let cut: String = paragraph.chars().take(PART_CHARS).collect();
            paragraph = paragraph.chars().skip(PART_CHARS).collect();
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            parts.push(cut);
        }
        if paragraph.is_empty() {
            continue;
        }
        if current.chars().count() + paragraph.chars().count() + 2 > PART_CHARS
            && !current.is_empty()
        {
            parts.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(&paragraph);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts.truncate(MAX_PARTS);
    parts
}

fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.extend(line.chars().filter(|c| !c.is_control() || *c == '\t'));
    }
    out
}

fn docx_text(bytes: &[u8]) -> Result<String, String> {
    let unreadable = || "This Word file could not be read.".to_owned();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| unreadable())?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|_| unreadable())?
        .take(64 * 1024 * 1024)
        .read_to_string(&mut xml)
        .map_err(|_| unreadable())?;
    Ok(word_xml_text(&xml))
}

/// Text runs of WordprocessingML, one paragraph per line.
fn word_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        let tag = &rest[start + 1..start + end];
        let after = &rest[start + end + 1..];
        let name = tag.split([' ', '/']).next().unwrap_or_default();
        match name {
            "w:t" if !tag.ends_with('/') => {
                if let Some(close) = after.find("</w:t>") {
                    out.push_str(&unescape(&after[..close]));
                    rest = &after[close + "</w:t>".len()..];
                    continue;
                }
            }
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" => out.push('\n'),
            _ if tag == "/w:p" => out.push_str("\n\n"),
            _ => {}
        }
        rest = after;
    }
    out
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn docx(xml: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut bytes));
            zip.start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn kinds_follow_the_extension() {
        assert_eq!(kind_of("Agenda.PDF"), Some(DocumentKind::Pdf));
        assert_eq!(kind_of("notes.docx"), Some(DocumentKind::Word));
        assert_eq!(kind_of("a.b.md"), Some(DocumentKind::Text));
        assert_eq!(kind_of("photo.jpeg"), Some(DocumentKind::Image));
        assert_eq!(kind_of("old.doc"), None);
        assert_eq!(kind_of("noextension"), None);
    }

    #[test]
    fn word_paragraphs_runs_and_entities_become_text() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Budget</w:t></w:r><w:r><w:t xml:space="preserve"> is 40k &amp; fixed</w:t></w:r></w:p><w:p><w:r><w:t>Owner:</w:t><w:tab/><w:t>Ana</w:t></w:r></w:p><w:p/></w:body></w:document>"#;
        let text = extract_text(&DocumentKind::Word, &docx(xml)).unwrap();
        assert_eq!(text, "Budget is 40k & fixed\n\nOwner:\tAna");
        assert!(extract_text(&DocumentKind::Word, b"not a zip").is_err());
    }

    #[test]
    fn plain_text_is_tidied_and_empty_files_are_refused() {
        let text = extract_text(&DocumentKind::Text, b"one\r\n\r\n\r\ntwo  \nthree\x07").unwrap();
        assert_eq!(text, "one\n\ntwo\nthree");
        assert!(extract_text(&DocumentKind::Text, b" \n ").is_err());
        assert!(extract_text(&DocumentKind::Pdf, b"%PDF-broken").is_err());
    }

    #[test]
    fn long_text_splits_on_paragraphs_within_the_part_size() {
        let paragraph = "word ".repeat(300);
        let text = [paragraph.trim(); 10].join("\n\n");
        let parts = split_parts(&text);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| part.chars().count() <= PART_CHARS));
        let huge = "x".repeat(PART_CHARS * 2 + 10);
        assert_eq!(split_parts(&huge).len(), 3);
    }

    /// A one-page PDF with real text, built with correct cross-references.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 18 Tf 20 100 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{object}\nendobj\n", index + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        pdf
    }

    #[test]
    fn pdf_text_is_extracted() {
        let text = extract_text(&DocumentKind::Pdf, &tiny_pdf("Budget cap is 40k")).unwrap();
        assert!(text.contains("Budget cap is 40k"), "{text:?}");
    }
}
