//! Inert Markdown presentation: no HTML execution, URL dispatch, or image fetches.
//! Parsed blocks are cached per message, so unchanged answers are not re-parsed
//! during the microphone's frequent progress repaints. Incomplete streams remain
//! valid CommonMark and are replaced in the cache as their text grows.
use eframe::egui::{self, Align, FontSelection, RichText, Ui};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct MarkdownCache {
    entries: BTreeMap<usize, (String, Vec<Block>)>,
}
impl MarkdownCache {
    pub fn show(&mut self, ui: &mut Ui, key: usize, text: &str) {
        let strong_family = egui::FontFamily::Name("PhorminxStrong".into());
        let has_strong_font = ui.fonts(|fonts| fonts.families().contains(&strong_family));
        let entry = self.entries.entry(key).or_default();
        if entry.0 != text {
            *entry = (text.to_owned(), parse(text));
        }
        for block in &entry.1 {
            if block.rule {
                ui.separator();
                continue;
            }
            let mut job = egui::text::LayoutJob::default();
            for span in &block.spans {
                let mut rich = RichText::new(&span.text);
                if span.strong {
                    rich = rich.strong();
                    if has_strong_font && !span.code && !block.code {
                        rich = rich.family(strong_family.clone());
                    }
                }
                if span.italic {
                    rich = rich.italics();
                }
                if span.strike {
                    rich = rich.strikethrough();
                }
                if span.code || block.code {
                    rich = rich.monospace();
                }
                if block.heading > 0 {
                    rich = rich
                        .size(24.0 - f32::from(block.heading.min(5)) * 1.5)
                        .strong();
                    if has_strong_font {
                        rich = rich.family(strong_family.clone());
                    }
                }
                if block.quote {
                    rich = rich.color(ui.visuals().weak_text_color());
                }
                rich.append_to(&mut job, ui.style(), FontSelection::Default, Align::Min);
            }
            job.wrap.max_width = ui.available_width();
            if block.code {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.add(egui::Label::new(job).wrap().selectable(true));
                });
            } else {
                ui.add(egui::Label::new(job).wrap().selectable(true));
            }
            ui.add_space(5.0);
        }
    }
    pub fn retain(&mut self, count: usize) {
        self.entries.retain(|key, _| *key < count);
    }
}

#[derive(Clone, Debug, Default)]
struct Span {
    text: String,
    strong: bool,
    italic: bool,
    strike: bool,
    code: bool,
}
#[derive(Clone, Debug, Default)]
struct Block {
    spans: Vec<Span>,
    heading: u8,
    code: bool,
    quote: bool,
    rule: bool,
}
fn flush(block: &mut Block, blocks: &mut Vec<Block>) {
    if !block.spans.is_empty() || block.rule {
        blocks.push(std::mem::take(block));
    }
}
fn parse(text: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut blocks = Vec::new();
    let mut block = Block::default();
    let mut style = Span::default();
    let mut lists: Vec<Option<u64>> = vec![];
    let mut quotes = 0usize;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut block, &mut blocks);
                block.heading = level as u8;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut block, &mut blocks);
                block.code = true;
            }
            Event::Start(Tag::Paragraph) => {
                block.quote = quotes > 0;
            }
            Event::Start(Tag::Strong) => style.strong = true,
            Event::End(TagEnd::Strong) => style.strong = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strikethrough) => style.strike = true,
            Event::End(TagEnd::Strikethrough) => style.strike = false,
            Event::Start(Tag::List(start)) => {
                flush(&mut block, &mut blocks);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                flush(&mut block, &mut blocks);
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                flush(&mut block, &mut blocks);
                let indent = "  ".repeat(lists.len().saturating_sub(1));
                let prefix = match lists.last_mut() {
                    Some(Some(index)) => {
                        let prefix = format!("{indent}{index}. ");
                        *index = index.saturating_add(1);
                        prefix
                    }
                    _ => format!("{indent}• "),
                };
                block.spans.push(Span {
                    text: prefix,
                    ..Default::default()
                });
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush(&mut block, &mut blocks);
                quotes += 1;
                block.quote = true;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut block, &mut blocks);
                quotes = quotes.saturating_sub(1);
            }
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::TableHead
                | TagEnd::TableRow,
            ) => flush(&mut block, &mut blocks),
            Event::End(TagEnd::TableCell) => block.spans.push(Span {
                text: "   │   ".into(),
                ..Default::default()
            }),
            Event::Text(value) | Event::Html(value) | Event::InlineHtml(value) => {
                block.spans.push(Span {
                    text: value.into_string(),
                    ..style.clone()
                })
            }
            Event::Code(value) => block.spans.push(Span {
                text: value.into_string(),
                code: true,
                ..style.clone()
            }),
            Event::SoftBreak => block.spans.push(Span {
                text: " ".into(),
                ..style.clone()
            }),
            Event::HardBreak => block.spans.push(Span {
                text: "\n".into(),
                ..style.clone()
            }),
            Event::Rule => {
                flush(&mut block, &mut blocks);
                block.rule = true;
                flush(&mut block, &mut blocks);
            }
            Event::TaskListMarker(checked) => block.spans.push(Span {
                text: if checked { "☑ " } else { "☐ " }.into(),
                ..Default::default()
            }),
            Event::Start(Tag::Image { .. }) => block.spans.push(Span {
                text: "[Image: ".into(),
                ..Default::default()
            }),
            Event::End(TagEnd::Image) => block.spans.push(Span {
                text: "]".into(),
                ..Default::default()
            }),
            _ => {}
        }
    }
    flush(&mut block, &mut blocks);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rich_content_and_incomplete_stream_are_preserved() {
        let blocks = parse(
            "# Heading\n\n**bold** and *soft* and `code`\n\n1. One\n2. Two\n\n```rust\nlet a = 2;\n```",
        );
        assert_eq!(blocks[0].heading, 1);
        assert!(
            blocks
                .iter()
                .flat_map(|b| &b.spans)
                .any(|s| s.strong && s.text == "bold")
        );
        assert!(
            blocks
                .iter()
                .flat_map(|b| &b.spans)
                .any(|s| s.italic && s.text == "soft")
        );
        assert!(blocks.iter().any(|b| b.code));
        assert!(!parse("An unfinished **answer").is_empty());
    }
    #[test]
    fn image_and_html_are_inert_text_and_cache_is_bounded_by_messages() {
        let blocks = parse("![private](https://attacker.test/beacon)\n\n<script>run()</script>");
        let rendered: String = blocks
            .iter()
            .flat_map(|b| &b.spans)
            .map(|s| s.text.as_str())
            .collect();
        assert!(rendered.contains("[Image: private]"));
        assert!(!rendered.contains("attacker.test"));
        assert!(rendered.contains("<script>"));
        let mut cache = MarkdownCache::default();
        egui::__run_test_ui(|ui| {
            cache.show(ui, 0, "**First**");
            cache.show(ui, 1, "Second");
        });
        cache.retain(1);
        assert_eq!(cache.entries.len(), 1);
        egui::__run_test_ui(|ui| cache.show(ui, 0, "**Changed**"));
        assert_eq!(cache.entries[&0].0, "**Changed**");
    }
}
