//! Client-side Open Graph unfurls for http(s) links in message bubbles.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug)]
pub struct Preview {
    pub url: String,
    pub host: String,
    pub title: String,
    pub description: String,
    pub image_path: Option<PathBuf>,
}

static CACHE: OnceLock<Mutex<HashMap<String, Option<Preview>>>> = OnceLock::new();
static INFLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Option<Preview>>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn inflight() -> &'static Mutex<HashSet<String>> {
    INFLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Returns true if this caller should start a fetch (false if already running).
pub fn begin_fetch(url: &str) -> bool {
    inflight().lock().map(|mut s| s.insert(url.to_string())).unwrap_or(true)
}

pub fn end_fetch(url: &str) {
    if let Ok(mut s) = inflight().lock() {
        s.remove(url);
    }
}

pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let t = token.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ')' | '(' | '"' | '\'' | '!' | '?' | ']'));
        if (t.starts_with("https://") || t.starts_with("http://"))
            && let Some(clean) = sanitize_url(t)
            && !out.contains(&clean)
        {
            out.push(clean);
        }
    }
    out
}

fn sanitize_url(raw: &str) -> Option<String> {
    let u = raw.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return None;
    }
    // Drop credentials if present.
    if let Some(at) = u.find('@')
        && let Some(scheme) = u.find("://")
        && at > scheme
    {
        let rest = &u[at + 1..];
        let scheme_s = &u[..scheme + 3];
        return Some(format!("{scheme_s}{rest}"));
    }
    Some(u.to_string())
}

pub fn cached(url: &str) -> Option<Option<Preview>> {
    cache().lock().ok()?.get(url).cloned()
}

pub fn store(url: &str, preview: Option<Preview>) {
    if let Ok(mut g) = cache().lock() {
        g.insert(url.to_string(), preview);
    }
}

/// Blocking fetch — call from a worker thread.
pub fn fetch(url: &str) -> Option<Preview> {
    let url = sanitize_url(url)?;
    let host = host_of(&url).unwrap_or_else(|| "link".into());
    let (final_url, body) = fetch_capped(&url, 1_500_000)?;
    let html = String::from_utf8_lossy(&body).into_owned();
    let title = meta(&html, "og:title").or_else(|| title_tag(&html)).unwrap_or_else(|| host.clone());
    let description = meta(&html, "og:description").unwrap_or_default();
    // Relative og:image paths are relative to the page we ended up on.
    let image_url = meta(&html, "og:image").map(|iu| absolutize(&final_url, &iu));
    let image_path = image_url.as_ref().and_then(|iu| download_image(iu));
    Some(Preview { url, host, title: truncate(&title, 120), description: truncate(&description, 180), image_path })
}

pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split('/').next()?.split('@').next_back()?;
    Some(host.trim_start_matches("www.").to_string())
}

fn absolutize(base: &str, maybe: &str) -> String {
    if maybe.starts_with("https://") || maybe.starts_with("http://") {
        return maybe.to_string();
    }
    if let Some(scheme) = base.find("://") {
        let after = &base[scheme + 3..];
        if let Some(slash) = after.find('/') {
            let origin = &base[..scheme + 3 + slash];
            if maybe.starts_with("//") {
                return format!("{}:{maybe}", &base[..scheme]);
            }
            if maybe.starts_with('/') {
                return format!("{origin}{maybe}");
            }
            return format!("{origin}/{maybe}");
        }
    }
    maybe.to_string()
}

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// (host, port) of an http(s) URL; strips userinfo and IPv6 brackets.
fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let default = if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 };
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default);
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => Some((h.to_string(), p.parse().ok()?)),
        _ => Some((authority.to_string(), default)),
    }
}

/// Loopback, private, link-local, CGNAT, multicast, unspecified — never fetched.
fn is_internal_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal_ip(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00 // unique local
                || (seg0 & 0xffc0) == 0xfe80 // link local
        }
    }
}

/// Resolve `url`'s host to one public address to pin curl to, or None if the host
/// is (or resolves to) anything on this machine or the local network.
fn public_target(url: &str) -> Option<(String, u16, std::net::IpAddr)> {
    use std::net::ToSocketAddrs;
    let (host, port) = host_port(url)?;
    let lower = host.to_ascii_lowercase();
    if lower.is_empty()
        || lower == "localhost"
        || [".localhost", ".local", ".lan", ".internal", ".home.arpa", ".home"].iter().any(|s| lower.ends_with(s))
    {
        return None;
    }
    let addrs: Vec<_> = (lower.as_str(), port).to_socket_addrs().ok()?.collect();
    if addrs.is_empty() || addrs.iter().any(|a| is_internal_ip(a.ip())) {
        return None;
    }
    Some((host, port, addrs[0].ip()))
}

/// GET `url` with curl, following up to 5 redirects by hand so every hop is checked
/// against [`public_target`] and pinned to the checked address (no DNS rebinding).
/// URL globbing is off, and at most `max` body bytes are read; larger bodies are
/// cut off there. Returns the final URL and the body.
fn fetch_capped(url: &str, max: usize) -> Option<(String, Vec<u8>)> {
    use std::io::Read;
    let mut url = url.to_string();
    for _ in 0..6 {
        let (host, port, ip) = public_target(&url)?;
        let ip_s = match ip {
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        };
        let mut child = std::process::Command::new("curl")
            .args([
                "-sS",
                "--globoff",
                "--max-time",
                "8",
                "--proto",
                "=https,http",
                "--compressed",
                "-A",
                UA,
                "--resolve",
                &format!("{host}:{port}:{ip_s}"),
                "-w",
                "%{stderr}\n@@redirect=%{redirect_url}",
                "--",
                &url,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let mut body = Vec::new();
        let read = child.stdout.take()?.take(max as u64 + 1).read_to_end(&mut body);
        if read.is_err() || body.len() > max {
            // Over the cap: keep what fits (enough for <head> meta tags).
            let _ = child.kill();
            let _ = child.wait();
            body.truncate(max);
            return (!body.is_empty()).then_some((url, body));
        }
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = child.wait();
        let redirect = err.rsplit("@@redirect=").next().map(str::trim).unwrap_or("");
        if !redirect.is_empty() && err.contains("@@redirect=") {
            url = sanitize_url(redirect)?;
            continue;
        }
        return (!body.is_empty()).then_some((url, body));
    }
    None
}

fn download_image(url: &str) -> Option<PathBuf> {
    let url = sanitize_url(url)?;
    // Skip real SVG assets only (path ends with .svg). Wikipedia thumbs look like
    // `…/Wikipedia-logo-v2.svg/250px-….png` — those are PNG and must not be skipped.
    let path_part = url.split('?').next().unwrap_or(&url).to_ascii_lowercase();
    if path_part.ends_with(".svg") {
        return None;
    }
    let dir = nyx_core::paths::cache_dir().join("nyx-messages").join("link-previews");
    let _ = std::fs::create_dir_all(&dir);
    let hash = url.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    for ext in ["png", "jpg", "jpeg", "gif", "webp"] {
        let p = dir.join(format!("{hash:x}.{ext}"));
        if p.exists() && p.metadata().map(|m| m.len() > 64).unwrap_or(false) {
            return Some(p);
        }
    }
    const MAX_IMAGE: usize = 2 * 1024 * 1024;
    let (_, bytes) = fetch_capped(&url, MAX_IMAGE + 1)?;
    if bytes.len() < 24 || bytes.len() > MAX_IMAGE {
        return None; // too small to be an image, or cut off
    }
    // Reject SVG masquerading as an image download.
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).to_ascii_lowercase();
    if head.contains("<svg") {
        return None;
    }
    let ext = image_ext(&bytes)?;
    // WebP only when GTK's own loaders can decode it (no external converters).
    if ext == "webp" && !gdk_can_load("webp") {
        return None;
    }
    let path = dir.join(format!("{hash:x}.{ext}"));
    std::fs::write(&path, &bytes).ok()?;
    Some(path)
}

fn gdk_can_load(format: &str) -> bool {
    static FORMATS: OnceLock<Vec<String>> = OnceLock::new();
    FORMATS
        .get_or_init(|| gtk::gdk_pixbuf::Pixbuf::formats().iter().filter_map(|f| f.name()).map(|n| n.to_string()).collect())
        .iter()
        .any(|n| n == format)
}

fn image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("png")
    } else if bytes.len() > 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        Some("jpg")
    } else if bytes.len() > 5 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        Some("gif")
    } else if bytes.len() > 11 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn meta(html: &str, prop: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    // Find a <meta …> that mentions this property/name, then read content=.
    let keys =
        [format!("property=\"{prop}\""), format!("property='{prop}'"), format!("name=\"{prop}\""), format!("name='{prop}'")];
    let mut search_from = 0;
    while search_from < lower.len() {
        let mut hit: Option<usize> = None;
        for key in &keys {
            if let Some(i) = lower[search_from..].find(key) {
                let abs = search_from + i;
                hit = Some(match hit {
                    Some(h) => h.min(abs),
                    None => abs,
                });
            }
        }
        let Some(idx) = hit else {
            break;
        };
        // Walk back to the start of this tag.
        let tag_start = lower[..idx].rfind("<meta").unwrap_or(idx);
        let tag_end = lower[tag_start..].find('>').map(|o| tag_start + o).unwrap_or_else(|| (tag_start + 400).min(html.len()));
        let tag = &html[tag_start..tag_end];
        if let Some(val) = content_attr(tag) {
            return Some(val);
        }
        search_from = idx + 1;
    }
    None
}

fn content_attr(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let cidx = lower.find("content=")?;
    let after = &tag[cidx + "content=".len()..];
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after[1..];
    let end = rest.find(quote)?;
    let val = html_decode(&rest[..end]).trim().to_string();
    if val.is_empty() { None } else { Some(val) }
}

fn title_tag(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let gt = html[start..].find('>')? + start + 1;
    let end = lower[gt..].find("</title>")? + gt;
    let val = html_decode(&html[gt..end]).trim().to_string();
    if val.is_empty() { None } else { Some(val) }
}

fn html_decode(s: &str) -> String {
    let named = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    let chars: Vec<char> = named.chars().collect();
    let mut result = String::with_capacity(named.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&' && i + 2 < chars.len() && chars[i + 1] == '#' {
            let mut j = i + 2;
            let hex = chars[j] == 'x' || chars[j] == 'X';
            if hex {
                j += 1;
            }
            let start_digits = j;
            while j < chars.len() {
                let c = chars[j];
                let ok = if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() };
                if !ok {
                    break;
                }
                j += 1;
            }
            if j < chars.len() && chars[j] == ';' && j > start_digits {
                let num: String = chars[start_digits..j].iter().collect();
                let code = if hex { u32::from_str_radix(&num, 16).ok() } else { num.parse().ok() };
                if let Some(c) = code.and_then(char::from_u32) {
                    result.push(c);
                    i = j + 1;
                    continue;
                }
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

fn truncate(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if s.chars().count() > max { format!("{t}…") } else { t }
}

/// Escape for Pango markup and wrap URLs in links.
pub fn linkify_markup(text: &str) -> String {
    let mut out = String::new();
    for (i, token) in text.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let trim_end: String = token
            .chars()
            .rev()
            .take_while(|c| matches!(c, ',' | '.' | ';' | ')' | '!' | '?' | ']'))
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let core = if trim_end.is_empty() { token } else { &token[..token.len() - trim_end.len()] };
        if (core.starts_with("https://") || core.starts_with("http://"))
            && let Some(clean) = sanitize_url(core)
        {
            out.push_str(&format!("<a href=\"{}\">{}</a>{}", escape_attr(&clean), escape_text(core), escape_text(&trim_end)));
        } else {
            out.push_str(&escape_text(token));
        }
    }
    out
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_hosts_are_refused() {
        for u in [
            "http://localhost/x",
            "http://127.0.0.1:8080/",
            "http://192.168.1.1/admin",
            "http://10.0.0.5/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
            "http://printer.local/",
            "http://user@192.168.0.2/",
        ] {
            assert!(public_target(u).is_none(), "{u}");
        }
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(host_port("https://a.b/c").unwrap(), ("a.b".into(), 443));
        assert_eq!(host_port("http://a.b:81/c?d").unwrap(), ("a.b".into(), 81));
        assert_eq!(host_port("http://u:p@a.b/").unwrap(), ("a.b".into(), 80));
        assert_eq!(host_port("http://[fe80::1]:9/").unwrap(), ("fe80::1".into(), 9));
    }
}

#[cfg(test)]
mod net_tests {
    /// Hits the network: `cargo test -- --ignored net_`
    #[test]
    #[ignore]
    fn net_fetch_follows_redirects_and_reads_meta() {
        let p = super::fetch("http://github.com").expect("github preview");
        assert!(p.title.to_lowercase().contains("github"), "{}", p.title);
        assert!(p.image_path.is_some(), "github og:image");
        let w = super::fetch("https://www.wikipedia.org").expect("wikipedia preview");
        assert!(w.title.to_lowercase().contains("wikipedia"), "{}", w.title);
        assert!(super::fetch("https://example.com/[1-99999]").map(|p| p.host).is_some());
    }
}
