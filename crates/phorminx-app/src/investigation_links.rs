//! One-time link previews for investigation link cards.
//!
//! A pasted URL is fetched once, in the background: title, description, site
//! name, favicon and preview image. Requests carry no cookies or credentials,
//! follow a few redirects, and are bounded in time and size. Hosts that name
//! this computer or a private network are never fetched. Failure leaves a
//! plain URL card.

use std::io::Read;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use phorminx_persistence::{LinkPreviewUpdate, Persistence};
use url::Url;

use crate::investigation_ocr::{WorkerSignal, now_ms};
use crate::investigations::{StoredImage, store_preview_image};

const MAX_PAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PREVIEW_IMAGE_BYTES: u64 = 6 * 1024 * 1024;
const MAX_FAVICON_BYTES: u64 = 512 * 1024;
const PAGE_TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Phorminx/0.1 link-preview";

/// Normalizes user input into a fetchable URL, or explains why it is not one.
pub fn parse_link(text: &str) -> Option<Url> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    let candidate = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else if trimmed.starts_with("www.") && trimmed[4..].contains('.') {
        format!("https://{trimmed}")
    } else {
        return None;
    };
    let url = Url::parse(&candidate).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

/// True when the URL names this computer or a private/link-local network.
pub fn is_local_target(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost"
                || domain.ends_with(".localhost")
                || domain.ends_with(".local")
                || domain.ends_with(".internal")
                || !domain.contains('.')
        }
        Some(url::Host::Ipv4(ip)) => is_private_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => is_private_ip(IpAddr::V6(ip)),
        None => true,
    }
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1])
                || ip.octets()[0] == 0
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_private_ip(IpAddr::V4(v4)))
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub image: Option<Url>,
    pub icon: Option<Url>,
}

/// Extracts preview metadata from the start of an HTML document. Only
/// `<title>`, `<meta>` and `<link>` tags are read; nothing is executed.
pub fn parse_metadata(html: &str, base: &Url) -> PageMetadata {
    let head_end = find_ascii_ci(html, "</head").unwrap_or(html.len());
    let head = &html[..head_end];
    let mut meta = PageMetadata::default();
    let mut og_title = None;
    let mut og_description = None;
    let mut twitter_image = None;
    let mut icons: Vec<(u8, Url)> = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = head[cursor..].find('<') {
        let start = cursor + offset;
        let Some(end_offset) = head[start..].find('>') else {
            break;
        };
        let tag = &head[start + 1..start + end_offset];
        cursor = start + end_offset + 1;
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match name.as_str() {
            "title" if meta.title.is_none() => {
                if let Some(close) = find_ascii_ci(&head[cursor..], "</title") {
                    meta.title = clean_text(&head[cursor..cursor + close]);
                }
            }
            "meta" => {
                let attributes = parse_attributes(tag);
                let key = attribute(&attributes, "property")
                    .or_else(|| attribute(&attributes, "name"))
                    .map(str::to_ascii_lowercase);
                let Some(content) = attribute(&attributes, "content") else {
                    continue;
                };
                match key.as_deref() {
                    Some("og:title") => og_title = clean_text(content),
                    Some("og:description") => og_description = clean_text(content),
                    Some("description") if meta.description.is_none() => {
                        meta.description = clean_text(content)
                    }
                    Some("og:site_name") => meta.site_name = clean_text(content),
                    Some("og:image") | Some("og:image:url") | Some("og:image:secure_url")
                        if meta.image.is_none() =>
                    {
                        meta.image = resolve(base, content)
                    }
                    Some("twitter:image") | Some("twitter:image:src") => {
                        twitter_image = resolve(base, content)
                    }
                    _ => {}
                }
            }
            "link" => {
                let attributes = parse_attributes(tag);
                let rel = attribute(&attributes, "rel")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let Some(href) = attribute(&attributes, "href") else {
                    continue;
                };
                let rank = if rel.split_whitespace().any(|r| r == "apple-touch-icon") {
                    3
                } else if rel.split_whitespace().any(|r| r == "icon") {
                    if href.to_ascii_lowercase().ends_with(".svg") {
                        0
                    } else {
                        2
                    }
                } else {
                    continue;
                };
                if rank > 0
                    && let Some(url) = resolve(base, href)
                {
                    icons.push((rank, url));
                }
            }
            _ => {}
        }
    }
    meta.title = og_title.or(meta.title);
    meta.description = og_description.or(meta.description);
    meta.image = meta.image.or(twitter_image);
    icons.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    meta.icon = icons
        .into_iter()
        .map(|(_, url)| url)
        .next()
        .or_else(|| base.join("/favicon.ico").ok());
    meta
}

fn resolve(base: &Url, value: &str) -> Option<Url> {
    let url = base.join(decode_entities(value.trim()).as_str()).ok()?;
    (matches!(url.scheme(), "http" | "https") && !is_local_target(&url)).then_some(url)
}

fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

fn parse_attributes(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let mut attributes = Vec::new();
    let mut index = bytes
        .iter()
        .position(|b| b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'/'
        {
            index += 1;
        }
        if name_start == index {
            index += 1;
            continue;
        }
        let name = tag[name_start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let mut value = String::new();
        if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index < bytes.len() && (bytes[index] == b'"' || bytes[index] == b'\'') {
                let quote = bytes[index];
                let value_start = index + 1;
                index = value_start;
                while index < bytes.len() && bytes[index] != quote {
                    index += 1;
                }
                value = tag[value_start..index.min(bytes.len())].to_owned();
                index += 1;
            } else {
                let value_start = index;
                while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                value = tag[value_start..index].to_owned();
            }
        }
        attributes.push((name, value));
    }
    attributes
}

fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn clean_text(value: &str) -> Option<String> {
    let text = decode_entities(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let text: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(1000)
        .collect();
    (!text.is_empty()).then_some(text)
}

fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            "hellip" => Some('…'),
            "rsquo" => Some('’'),
            "lsquo" => Some('‘'),
            "ldquo" => Some('“'),
            "rdquo" => Some('”'),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decodes page bytes. UTF-8 is the default; Latin-1/Windows-1252 pages are
/// mapped byte-for-byte, which is exact for their printable range.
fn decode_page(bytes: &[u8], content_type: &str) -> String {
    let declared = content_type.to_ascii_lowercase();
    let sniff = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
    let latin = ["iso-8859-1", "windows-1252", "latin1"];
    if latin.iter().any(|name| declared.contains(name))
        || (!declared.contains("utf-8")
            && latin.iter().any(|name| {
                sniff.contains(&format!("charset={name}"))
                    || sniff.contains(&format!("charset=\"{name}"))
            })
            && std::str::from_utf8(bytes).is_err())
    {
        return bytes.iter().map(|&b| b as char).collect();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Refuses public-looking names that resolve to this computer or a private
/// network (DNS rebinding). The connection resolves again; this is a
/// best-effort guard, not a sandbox.
fn resolves_to_local(url: &Url) -> bool {
    use std::net::ToSocketAddrs;
    let Some(host) = url.host_str() else {
        return true;
    };
    let port = url.port_or_known_default().unwrap_or(443);
    match (host.trim_start_matches('[').trim_end_matches(']'), port).to_socket_addrs() {
        Ok(addresses) => addresses
            .into_iter()
            .any(|address| is_private_ip(address.ip())),
        Err(_) => true,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(PAGE_TIMEOUT))
        // Redirects are followed manually so every hop is checked.
        .max_redirects(0)
        .http_status_as_error(true)
        .user_agent(USER_AGENT)
        .build()
        .into()
}

const MAX_REDIRECTS: usize = 5;

fn fetch(
    agent: &ureq::Agent,
    url: &Url,
    limit: u64,
    accept: &str,
) -> Option<(Vec<u8>, String, Url)> {
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        if is_local_target(&current) || resolves_to_local(&current) {
            return None;
        }
        let response = agent
            .get(current.as_str())
            .header("Accept", accept)
            .call()
            .ok()?;
        if response.status().is_redirection() {
            let location = response.headers().get("location")?.to_str().ok()?;
            current = current.join(location).ok()?;
            if !matches!(current.scheme(), "http" | "https") {
                return None;
            }
            continue;
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > limit {
            // Preview metadata lives in the document head, so a long page is
            // still useful; a truncated image or icon is not.
            if !content_type.to_ascii_lowercase().contains("html") {
                return None;
            }
            bytes.truncate(limit as usize);
        }
        return Some((bytes, content_type, current));
    }
    None
}

/// Fetches and stores a preview; `images` is the investigation's folder.
pub fn fetch_preview(url: &Url, images: &std::path::Path) -> Option<LinkPreviewUpdate> {
    let agent = agent();
    let (bytes, content_type, final_url) = fetch(
        &agent,
        url,
        MAX_PAGE_BYTES,
        "text/html,application/xhtml+xml;q=0.9,image/*;q=0.8,*/*;q=0.5",
    )?;
    let site = final_url
        .host_str()
        .map(|host| host.trim_start_matches("www.").to_owned());
    if content_type.to_ascii_lowercase().starts_with("image/") {
        let image = store_preview_image(images, &bytes, 1600).ok();
        return Some(LinkPreviewUpdate {
            title: final_url
                .path_segments()
                .and_then(|mut segments| segments.next_back().map(str::to_owned))
                .filter(|name| !name.is_empty()),
            site,
            image: image.map(StoredImage::into_reference),
            ..Default::default()
        });
    }
    let html = decode_page(&bytes, &content_type);
    let meta = parse_metadata(&html, &final_url);
    let image = meta
        .image
        .as_ref()
        .and_then(|image| fetch(&agent, image, MAX_PREVIEW_IMAGE_BYTES, "image/*"))
        .and_then(|(bytes, _, _)| store_preview_image(images, &bytes, 1200).ok())
        .map(StoredImage::into_reference);
    let favicon = meta
        .icon
        .as_ref()
        .and_then(|icon| fetch(&agent, icon, MAX_FAVICON_BYTES, "image/*"))
        .and_then(|(bytes, _, _)| store_preview_image(images, &bytes, 64).ok())
        .map(|stored| stored.sha256);
    Some(LinkPreviewUpdate {
        title: meta.title,
        description: meta.description,
        site: meta.site_name.or(site),
        image,
        favicon_sha256: favicon,
    })
}

/// Background fetcher for pending link items, oldest first.
pub struct LinkWorker {
    signal: Arc<WorkerSignal>,
    thread: Option<JoinHandle<()>>,
}

impl LinkWorker {
    pub fn spawn(
        database: PathBuf,
        images: PathBuf,
        signal: Arc<WorkerSignal>,
        revision: Arc<AtomicU64>,
    ) -> Result<Self, String> {
        let thread_signal = signal.clone();
        let thread = thread::Builder::new()
            .name("phorminx-investigation-links".into())
            .spawn(move || {
                let Ok(db) = Persistence::open_with_busy_timeout(&database, Duration::from_secs(2))
                else {
                    return;
                };
                while !thread_signal.stopping() {
                    let Ok(Some(job)) = db.investigations().next_pending_fetch() else {
                        thread_signal.wait(Duration::from_secs(5));
                        continue;
                    };
                    let folder = images.join(job.investigation_id.to_string());
                    let preview = parse_link(&job.target)
                        .filter(|url| !is_local_target(url))
                        .and_then(|url| fetch_preview(&url, &folder));
                    let saved = match preview {
                        Some(preview) => {
                            db.investigations()
                                .save_link_preview(job.item_id, &preview, now_ms())
                        }
                        None => db.investigations().fail_fetch(job.item_id, now_ms()),
                    };
                    if saved.is_err() {
                        thread_signal.wait(Duration::from_secs(2));
                    }
                    revision.fetch_add(1, Ordering::AcqRel);
                }
            })
            .map_err(|_| "Link previews could not start.")?;
        Ok(Self {
            signal,
            thread: Some(thread),
        })
    }
}

impl Drop for LinkWorker {
    fn drop(&mut self) {
        self.signal.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_normalized_and_unsafe_forms_rejected() {
        assert_eq!(
            parse_link(" https://example.com/a?b=1 ").unwrap().as_str(),
            "https://example.com/a?b=1"
        );
        assert_eq!(
            parse_link("www.example.org/x").unwrap().as_str(),
            "https://www.example.org/x"
        );
        for rejected in [
            "",
            "example.com",
            "ftp://example.com",
            "javascript:alert(1)",
            "file:///C:/Windows",
            "https://user:pass@example.com",
            "https://exa mple.com",
            "two lines\nhttps://example.com",
        ] {
            assert!(parse_link(rejected).is_none(), "{rejected}");
        }
    }

    #[test]
    fn local_and_private_targets_are_never_fetched() {
        for local in [
            "http://localhost:11434",
            "http://127.0.0.1/",
            "http://10.0.0.8/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://router/",
            "http://printer.local/",
            "http://100.64.0.1/",
        ] {
            assert!(is_local_target(&Url::parse(local).unwrap()), "{local}");
        }
        for public in [
            "https://example.com/",
            "http://93.184.215.14/",
            "https://[2606:4700::1]/",
        ] {
            assert!(!is_local_target(&Url::parse(public).unwrap()), "{public}");
        }
    }

    #[test]
    fn metadata_prefers_open_graph_and_resolves_relative_urls() {
        let base = Url::parse("https://news.example.com/story/42").unwrap();
        let html = r#"<!doctype html><html><head>
            <meta charset="utf-8"><TITLE> Plain &amp; simple </TITLE>
            <meta name="description" content="Fallback description">
            <meta property='og:title' content="Open &quot;Graph&quot; title">
            <meta property="og:description" content="A &#x27;rich&#39; summary&hellip;">
            <meta property="og:site_name" content="Example News">
            <meta property="og:image" content="/img/cover.png?w=1200&amp;h=630">
            <link rel="icon" href="/icons/favicon.svg">
            <link rel="shortcut icon" href="favicon-32.png">
            </head><body><meta property="og:title" content="ignored body tag"></body></html>"#;
        let meta = parse_metadata(html, &base);
        assert_eq!(meta.title.as_deref(), Some("Open \"Graph\" title"));
        assert_eq!(meta.description.as_deref(), Some("A 'rich' summary…"));
        assert_eq!(meta.site_name.as_deref(), Some("Example News"));
        assert_eq!(
            meta.image.unwrap().as_str(),
            "https://news.example.com/img/cover.png?w=1200&h=630"
        );
        assert_eq!(
            meta.icon.unwrap().as_str(),
            "https://news.example.com/story/favicon-32.png"
        );
    }

    #[test]
    fn metadata_falls_back_to_title_description_and_root_favicon() {
        let base = Url::parse("https://example.org/a/b").unwrap();
        let meta = parse_metadata(
            "<html><head><title>Hello\n   world</title><meta name=description content=Short></head>",
            &base,
        );
        assert_eq!(meta.title.as_deref(), Some("Hello world"));
        assert_eq!(meta.description.as_deref(), Some("Short"));
        assert_eq!(
            meta.icon.unwrap().as_str(),
            "https://example.org/favicon.ico"
        );
        assert!(meta.image.is_none());
        // Preview images pointing at local addresses are dropped.
        let meta = parse_metadata(
            r#"<meta property="og:image" content="http://127.0.0.1/secret.png">"#,
            &base,
        );
        assert!(meta.image.is_none());
    }

    #[test]
    fn pages_decode_utf8_and_declared_latin1() {
        assert_eq!(
            decode_page("Café".as_bytes(), "text/html; charset=utf-8"),
            "Café"
        );
        assert_eq!(
            decode_page(b"Caf\xe9", "text/html; charset=ISO-8859-1"),
            "Café"
        );
        assert_eq!(
            decode_page(b"<meta charset=windows-1252>Caf\xe9", "text/html"),
            "<meta charset=windows-1252>Café"
        );
    }

    #[test]
    fn entity_decoding_is_bounded_and_tolerant() {
        assert_eq!(
            decode_entities("a &amp b &unknown; &#65;&#x42;"),
            "a &amp b &unknown; AB"
        );
        assert_eq!(decode_entities("&#xFFFFFFFF;"), "&#xFFFFFFFF;");
    }
}
