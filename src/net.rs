//! Resource loading: `file://`, `http(s)://` (via `ureq` with rustls) and `data:` URLs,
//! plus character decoding for HTML and CSS.

use std::io::Read;
use std::time::Duration;
use url::Url;

/// A fetched resource.
#[derive(Clone, Debug)]
pub struct Response {
    /// Final URL after redirects.
    pub url: Url,
    /// Body bytes.
    pub bytes: Vec<u8>,
    /// `Content-Type` header (or a guess from the file extension), lowercased.
    pub content_type: Option<String>,
}

/// User agent string sent with HTTP requests.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) browser-engine/0.1 Safari/537.36";

/// Maximum response size accepted (32 MiB).
const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Turns a command-line argument into a URL: existing paths become `file://` URLs and
/// bare host names get `https://`.
pub fn url_from_arg(arg: &str) -> Result<Url, String> {
    let path = std::path::Path::new(arg);
    if path.exists() {
        let abs = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        return Url::from_file_path(&abs).map_err(|_| format!("bad path {arg}"));
    }
    match Url::parse(arg) {
        Ok(u) => Ok(u),
        Err(_) => Url::parse(&format!("https://{arg}")).map_err(|e| e.to_string()),
    }
}

/// Fetches a URL.
pub fn fetch(url: &Url) -> Result<Response, String> {
    match url.scheme() {
        "file" => {
            let path = url.to_file_path().map_err(|_| format!("bad file URL {url}"))?;
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(Response {
                url: url.clone(),
                bytes,
                content_type: guess_type(url.path()),
            })
        }
        "http" | "https" => fetch_http(url),
        "data" => fetch_data(url),
        s => Err(format!("unsupported scheme {s}")),
    }
}

fn fetch_http(url: &Url) -> Result<Response, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(url.as_str())
        .header("User-Agent", USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,text/css,image/png,image/jpeg,image/gif,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{url}: HTTP {}", status.as_u16()));
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase());
    let mut bytes = Vec::new();
    resp.body_mut()
        .as_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{url}: {e}"))?;
    Ok(Response {
        url: url.clone(),
        bytes,
        content_type,
    })
}

fn fetch_data(url: &Url) -> Result<Response, String> {
    let s = url.as_str();
    let rest = &s[5..];
    let comma = rest.find(',').ok_or("malformed data URL")?;
    let meta = &rest[..comma];
    let payload = &rest[comma + 1..];
    let is_b64 = meta.ends_with(";base64");
    let mime = meta
        .trim_end_matches(";base64")
        .split(';')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let decoded = percent_decode(payload);
    let bytes = if is_b64 {
        base64_decode(&decoded).ok_or("bad base64 in data URL")?
    } else {
        decoded
    };
    Ok(Response {
        url: url.clone(),
        bytes,
        content_type: if mime.is_empty() { None } else { Some(mime) },
    })
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Decodes standard or URL-safe base64, ignoring whitespace.
pub fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for &c in input {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

fn guess_type(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            _ => return None,
        }
        .to_string(),
    )
}

/// Decodes a text resource. Uses the charset from `content_type`, a BOM, or a
/// `<meta charset>` prescan (for HTML); supports UTF-8 and windows-1252/latin1.
pub fn decode_text(bytes: &[u8], content_type: Option<&str>, is_html: bool) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let mut charset = content_type.and_then(|ct| ct.split("charset=").nth(1)).map(|c| {
        c.trim_matches(|ch: char| ch == '"' || ch == '\'' || ch.is_whitespace())
            .to_ascii_lowercase()
    });
    if charset.is_none() && is_html {
        charset = prescan_meta_charset(&bytes[..bytes.len().min(4096)]);
    }
    match charset.as_deref() {
        Some("iso-8859-1") | Some("latin1") | Some("windows-1252") | Some("us-ascii") | Some("cp1252") => {
            decode_windows_1252(bytes)
        }
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) if charset.is_none() => {
                // Not valid UTF-8 and no declared charset: assume windows-1252.
                let lossy = String::from_utf8_lossy(bytes);
                let bad = lossy.chars().filter(|&c| c == '\u{FFFD}').count();
                if bad > 8 {
                    decode_windows_1252(bytes)
                } else {
                    lossy.into_owned()
                }
            }
            Err(_) => String::from_utf8_lossy(bytes).into_owned(),
        },
    }
}

fn prescan_meta_charset(head: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(head).to_ascii_lowercase();
    let idx = s.find("charset=")?;
    let rest = &s[idx + 8..];
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn decode_windows_1252(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{02C6}',
        '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}', '\u{017D}', '\u{8F}', '\u{90}', '\u{2018}',
        '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}',
        '\u{203A}', '\u{0153}', '\u{9D}', '\u{017E}', '\u{0178}',
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls() {
        let r = fetch(&Url::parse("data:text/plain;base64,SGVsbG8=").unwrap()).unwrap();
        assert_eq!(r.bytes, b"Hello");
        assert_eq!(r.content_type.as_deref(), Some("text/plain"));
        let r = fetch(&Url::parse("data:,a%20b").unwrap()).unwrap();
        assert_eq!(r.bytes, b"a b");
    }

    #[test]
    fn file_urls_and_args() {
        let dir = std::env::temp_dir().join(format!("be-net-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.html");
        std::fs::write(&f, "<p>hi").unwrap();
        let u = url_from_arg(f.to_str().unwrap()).unwrap();
        assert_eq!(u.scheme(), "file");
        let r = fetch(&u).unwrap();
        assert_eq!(r.bytes, b"<p>hi");
        assert_eq!(r.content_type.as_deref(), Some("text/html"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(url_from_arg("example.com").unwrap().as_str(), "https://example.com/");
    }

    #[test]
    fn charset_decoding() {
        assert_eq!(
            decode_text(b"caf\xe9", Some("text/html; charset=ISO-8859-1"), true),
            "café"
        );
        assert_eq!(decode_text("café".as_bytes(), None, true), "café");
        assert_eq!(
            decode_text(b"<meta charset=windows-1252>\x93q\x94", None, true),
            "<meta charset=windows-1252>\u{201C}q\u{201D}"
        );
    }
}
