use arco_core::files;
use encoding_rs::{Encoding, WINDOWS_1252};
use rtf_grimoire::tokenizer::{Token, read_token};
use std::borrow::Cow;
use std::fmt::Write as _;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::Path;

const DOCUMENT_LIMIT: u64 = 2 * 1024 * 1024;
const RTF_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

pub(crate) enum Rich {
    Html(String),
    Rtf(String),
    Media {
        element: &'static str,
        mime: &'static str,
    },
}

impl Rich {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Html(_) => "HTML",
            Self::Rtf(_) => "RTF",
            Self::Media {
                element: "video", ..
            } => "Video",
            Self::Media { .. } => "Audio",
        }
    }
}

pub(super) fn read(path: &Path, metadata: &Metadata) -> io::Result<Option<Rich>> {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return Ok(None);
    };
    let extension = extension.to_ascii_lowercase();
    let media = match extension.as_str() {
        "wav" => ("audio", "audio/wav"),
        "mp3" => ("audio", "audio/mpeg"),
        "m4a" => ("audio", "audio/mp4"),
        "aac" => ("audio", "audio/aac"),
        "flac" => ("audio", "audio/flac"),
        "oga" | "ogg" | "opus" => ("audio", "audio/ogg"),
        "mp4" | "m4v" => ("video", "video/mp4"),
        "webm" => ("video", "video/webm"),
        "ogv" => ("video", "video/ogg"),
        "htm" | "html" | "xhtml" | "rtf" => {
            if metadata.len() > DOCUMENT_LIMIT {
                return Ok(None);
            }
            let mut data = Vec::new();
            File::open(path)?
                .take(DOCUMENT_LIMIT + 1)
                .read_to_end(&mut data)?;
            files::ensure_unchanged(path, metadata)?;
            if data.len() as u64 > DOCUMENT_LIMIT {
                return Ok(None);
            }
            return Ok(if extension == "rtf" {
                rtf_html(&data).map(Rich::Rtf)
            } else {
                Some(Rich::Html(sanitize_html(&data)))
            });
        }
        _ => return Ok(None),
    };
    Ok(Some(Rich::Media {
        element: media.0,
        mime: media.1,
    }))
}

fn sanitize_html(data: &[u8]) -> String {
    let text = if let Some((encoding, bom)) = Encoding::for_bom(data) {
        encoding.decode_without_bom_handling(&data[bom..]).0
    } else if let Ok(text) = std::str::from_utf8(data) {
        Cow::Borrowed(text)
    } else {
        WINDOWS_1252.decode_without_bom_handling(data).0
    };
    ammonia::Builder::default()
        .url_schemes(["data"].into_iter().collect())
        .url_relative(ammonia::UrlRelative::Deny)
        .attribute_filter(|tag, attr, value| {
            if attr == "href" || attr == "src" {
                return (tag == "img"
                    && attr == "src"
                    && [
                        "data:image/png;base64,",
                        "data:image/jpeg;base64,",
                        "data:image/gif;base64,",
                        "data:image/webp;base64,",
                        "data:image/bmp;base64,",
                    ]
                    .iter()
                    .any(|prefix| value.starts_with(prefix)))
                .then_some(Cow::Borrowed(value));
            }
            Some(Cow::Borrowed(value))
        })
        .clean(&text)
        .to_string()
}

#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    half_points: Option<i32>,
}

#[derive(Clone, Copy)]
struct RtfState {
    style: Style,
    ignored: bool,
    encoding: &'static Encoding,
    fallback: usize,
}

impl Default for RtfState {
    fn default() -> Self {
        Self {
            style: Style::default(),
            ignored: false,
            encoding: WINDOWS_1252,
            fallback: 1,
        }
    }
}

#[derive(Default)]
struct RtfOutput {
    bytes: Vec<u8>,
    unicode: Vec<u16>,
    skip: usize,
    html: String,
}

impl RtfOutput {
    fn flush(&mut self, state: RtfState) {
        if !self.unicode.is_empty() {
            let text = String::from_utf16_lossy(&self.unicode);
            self.unicode.clear();
            styled(&mut self.html, &text, state.style);
        }
        if !self.bytes.is_empty() {
            let text = state.encoding.decode_without_bom_handling(&self.bytes).0;
            styled(&mut self.html, &text, state.style);
            self.bytes.clear();
        }
    }

    fn text(&mut self, bytes: &[u8], state: RtfState) {
        let skipped = self.skip.min(bytes.len());
        self.skip -= skipped;
        if skipped != bytes.len() {
            if !self.unicode.is_empty() {
                self.flush(state);
            }
            self.bytes.extend_from_slice(&bytes[skipped..]);
        }
    }
}

fn rtf_html(mut data: &[u8]) -> Option<String> {
    data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    if !data.starts_with(b"{\\rtf") {
        return None;
    }
    let mut state = RtfState::default();
    let mut groups = Vec::new();
    let mut output = RtfOutput::default();
    while !data.is_empty() {
        let (remaining, token) = read_token(data).ok()?;
        data = remaining;
        match token {
            Token::StartGroup => {
                output.flush(state);
                if groups.len() == 256 {
                    return None;
                }
                groups.push(state);
            }
            Token::EndGroup => {
                output.flush(state);
                state = groups.pop()?;
                output.skip = 0;
            }
            _ if state.ignored => (),
            Token::Text(bytes) => output.text(&bytes, state),
            Token::ControlSymbol(symbol @ ('\\' | '{' | '}')) => {
                output.text(&[symbol as u8], state)
            }
            Token::ControlSymbol('*') => {
                output.flush(state);
                state.ignored = true;
            }
            Token::ControlWord { name, arg } if name == "u" => {
                let value = arg?;
                if !(-32768..=65535).contains(&value) {
                    return None;
                }
                if !output.bytes.is_empty() {
                    output.flush(state);
                }
                output.unicode.push(value as u16);
                output.skip = state.fallback;
            }
            Token::ControlWord { name, arg } => {
                output.flush(state);
                let enabled = arg != Some(0);
                match name.as_str() {
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "object"
                    | "filetbl" | "listtable" | "listoverridetable" | "header" | "footer" => {
                        state.ignored = true
                    }
                    "b" => state.style.bold = enabled,
                    "i" => state.style.italic = enabled,
                    "ul" => state.style.underline = enabled,
                    "ulnone" => state.style.underline = false,
                    "strike" => state.style.strike = enabled,
                    "plain" => state.style = Style::default(),
                    "fs" => state.style.half_points = arg.filter(|size| (8..=144).contains(size)),
                    "par" | "line" => output.html.push_str("<br>"),
                    "tab" => output.html.push('\t'),
                    "uc" => state.fallback = arg.unwrap_or(1).clamp(0, 16) as usize,
                    "ansicpg" => {
                        if let Some(code) = arg {
                            state.encoding = rtf_encoding(code).unwrap_or(WINDOWS_1252);
                        }
                    }
                    "emdash" => output.html.push('—'),
                    "endash" => output.html.push('–'),
                    "bullet" => output.html.push('•'),
                    "lquote" => output.html.push('‘'),
                    "rquote" => output.html.push('’'),
                    "ldblquote" => output.html.push('“'),
                    "rdblquote" => output.html.push('”'),
                    _ => (),
                }
            }
            _ => (),
        }
        if output.html.len() > RTF_OUTPUT_LIMIT {
            return None;
        }
    }
    groups
        .is_empty()
        .then(|| format!("<div class=\"rtf\">{}</div>", output.html))
}

fn rtf_encoding(code: i32) -> Option<&'static Encoding> {
    match code {
        932 => Some(encoding_rs::SHIFT_JIS),
        936 => Some(encoding_rs::GBK),
        949 => Some(encoding_rs::EUC_KR),
        950 => Some(encoding_rs::BIG5),
        65001 => Some(encoding_rs::UTF_8),
        _ => Encoding::for_label(format!("windows-{code}").as_bytes()),
    }
}

fn styled(html: &mut String, text: &str, style: Style) {
    if let Some(size) = style.half_points {
        let _ = write!(
            html,
            "<span style=\"font-size:{}pt\">",
            f64::from(size) / 2.0
        );
    }
    for (enabled, tag) in [
        (style.bold, "b"),
        (style.italic, "i"),
        (style.underline, "u"),
        (style.strike, "s"),
    ] {
        if enabled {
            let _ = write!(html, "<{tag}>");
        }
    }
    html.push_str(&ammonia::clean_text(text));
    for (enabled, tag) in [
        (style.strike, "s"),
        (style.underline, "u"),
        (style.italic, "i"),
        (style.bold, "b"),
    ] {
        if enabled {
            let _ = write!(html, "</{tag}>");
        }
    }
    if style.half_points.is_some() {
        html.push_str("</span>");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_keeps_structure_and_removes_active_and_external_content() {
        let html = sanitize_html(br#"<h1>Titel</h1><script>alert(1)</script><img src="https://example.com/t.png" onerror="alert(1)"><a href="file:///secret">Link</a>"#);
        assert!(html.contains("<h1>Titel</h1>") && html.contains(">Link</a>"));
        for forbidden in ["script", "alert", "https:", "file:"] {
            assert!(!html.contains(forbidden));
        }
    }

    #[test]
    fn rtf_formats_unicode_and_hides_metadata_and_objects() {
        let rtf = br#"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}\uc1 Text {\b gr\'f6\u223?er}\par \u-10179?\u-8704? {\object payload} <b>}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(
            html.contains("<b>")
                && html.contains("<br>")
                && html.contains("😀")
                && html.contains("&lt;b&gt;")
        );
        for forbidden in ["Arial", "payload", "?"] {
            assert!(!html.contains(forbidden));
        }
    }
}
