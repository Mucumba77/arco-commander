use arco_core::files;
use encoding_rs::{Encoding, WINDOWS_1252};
use rtf_grimoire::tokenizer::{Token, read_token};
use std::borrow::Cow;
use std::collections::BTreeMap;
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
    foreground: Option<[u8; 3]>,
    highlight: Option<[u8; 3]>,
    script: Option<&'static str>,
}

#[derive(Clone, Copy, Default)]
struct Paragraph {
    alignment: Option<&'static str>,
    first_line: i32,
    left: i32,
    right: i32,
    before: i32,
    after: i32,
    line_spacing: i32,
    multiple_spacing: bool,
}

impl Paragraph {
    fn opening_tag(self) -> String {
        let mut css = String::new();
        if let Some(alignment) = self.alignment {
            let _ = write!(css, "text-align:{alignment};");
        }
        for (name, twips) in [
            ("text-indent", self.first_line),
            ("margin-left", self.left),
            ("margin-right", self.right),
            ("padding-top", self.before),
            ("padding-bottom", self.after),
        ] {
            if twips != 0 {
                let _ = write!(css, "{name}:{}pt;", f64::from(twips) / 20.0);
            }
        }
        match self.line_spacing {
            0 => (),
            twips if self.multiple_spacing => {
                let _ = write!(css, "line-height:{};", f64::from(twips.abs()) / 240.0);
            }
            twips if twips < 0 => {
                let _ = write!(css, "line-height:{}pt;", f64::from(-twips) / 20.0);
            }
            twips => {
                let _ = write!(css, "line-height:max(1.2em,{}pt);", f64::from(twips) / 20.0);
            }
        }
        if css.is_empty() {
            "<div>".into()
        } else {
            format!("<div style=\"{css}\">")
        }
    }
}

#[derive(Clone, Copy)]
struct RtfState {
    style: Style,
    paragraph: Paragraph,
    ignored: bool,
    encoding: &'static Encoding,
    fallback: usize,
    in_table: bool,
    table_level: i32,
    list_label: bool,
}

impl Default for RtfState {
    fn default() -> Self {
        Self {
            style: Style::default(),
            paragraph: Paragraph::default(),
            ignored: false,
            encoding: WINDOWS_1252,
            fallback: 1,
            in_table: false,
            table_level: 1,
            list_label: false,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Merge {
    #[default]
    None,
    Start,
    Continue,
}

#[derive(Clone, Copy, Default)]
struct CellDefinition {
    right: i32,
    horizontal: Merge,
    vertical: Merge,
}

struct Cell {
    content: String,
    bounds: Option<(i32, i32)>,
    horizontal: Merge,
    vertical: Merge,
    colspan: usize,
    rowspan: usize,
}

#[derive(Default)]
struct Table {
    open: bool,
    row_open: bool,
    cell_open: bool,
    cell_start: usize,
    left_edge: i32,
    defining: bool,
    next_cell: CellDefinition,
    definitions: Vec<CellDefinition>,
    row: Vec<String>,
    rows: Vec<Vec<Cell>>,
    buffered_bytes: usize,
}

impl Table {
    fn begin_definition(&mut self) {
        self.defining = true;
        self.left_edge = 0;
        self.next_cell = CellDefinition::default();
        self.definitions.clear();
    }

    fn finish_row(&mut self) {
        let mut left = self.left_edge;
        let mut row: Vec<Cell> = Vec::new();
        for (index, content) in self.row.drain(..).enumerate() {
            let definition = self.definitions.get(index).copied().unwrap_or_default();
            let bounds = self.definitions.get(index).and_then(|definition| {
                let previous = left;
                left = definition.right;
                (left > previous).then_some((previous, left))
            });
            if definition.horizontal == Merge::Continue
                && let Some(previous) = row.last_mut()
                && previous.horizontal == Merge::Start
                && let (Some((first, boundary)), Some((next, last))) = (previous.bounds, bounds)
                && boundary == next
                && (definition.vertical == Merge::None || definition.vertical == previous.vertical)
            {
                previous.content.push_str(&content);
                previous.bounds = Some((first, last));
                previous.colspan += 1;
            } else {
                row.push(Cell {
                    content,
                    bounds,
                    horizontal: definition.horizontal,
                    vertical: definition.vertical,
                    colspan: 1,
                    rowspan: 1,
                });
            }
        }
        self.rows.push(row);
    }

    fn render(mut self, html: &mut String) {
        // A continuation can only extend a cell from the immediately preceding
        // row with the same boundaries and the same horizontal span.
        let mut previous = BTreeMap::<(i32, i32), (usize, usize)>::new();
        for row_index in 0..self.rows.len() {
            let (earlier, rows) = self.rows.split_at_mut(row_index);
            let mut current = BTreeMap::new();
            for (cell_index, cell) in rows[0].iter_mut().enumerate() {
                let Some(bounds) = cell.bounds else {
                    continue;
                };
                if cell.vertical == Merge::Continue
                    && let Some(&(row, index)) = previous.get(&bounds)
                    && earlier[row][index].colspan == cell.colspan
                {
                    let anchor = &mut earlier[row][index];
                    anchor.rowspan += 1;
                    anchor.content.push_str(&cell.content);
                    cell.content.clear();
                    cell.rowspan = 0;
                    current.insert(bounds, (row, index));
                } else if cell.vertical == Merge::Start {
                    current.insert(bounds, (row_index, cell_index));
                }
            }
            previous = current;
        }

        html.push_str("<table class=\"rtf-table\"><tbody>");
        for row in self.rows {
            html.push_str("<tr>");
            for cell in row.into_iter().filter(|cell| cell.rowspan != 0) {
                html.push_str("<td");
                if cell.colspan > 1 {
                    let _ = write!(html, " colspan=\"{}\"", cell.colspan);
                }
                if cell.rowspan > 1 {
                    let _ = write!(html, " rowspan=\"{}\"", cell.rowspan);
                }
                if let Some(width) = cell
                    .bounds
                    .and_then(|(left, right)| right.checked_sub(left))
                {
                    let _ = write!(
                        html,
                        " style=\"width:{}pt;\"",
                        f64::from(width.min(20000)) / 20.0
                    );
                }
                html.push('>');
                html.push_str(&cell.content);
                html.push_str("</td>");
                if html.len() > RTF_OUTPUT_LIMIT {
                    return;
                }
            }
            html.push_str("</tr>");
        }
        html.push_str("</tbody></table>");
    }
}

#[derive(Default)]
struct RtfOutput {
    bytes: Vec<u8>,
    unicode: Vec<u16>,
    skip: usize,
    html: String,
    paragraph_start: usize,
    paragraph: Paragraph,
    table: Table,
    list_open: bool,
    list_marker: String,
}

impl RtfOutput {
    fn flush(&mut self, state: RtfState) {
        if !self.unicode.is_empty() {
            let text = String::from_utf16_lossy(&self.unicode);
            self.unicode.clear();
            self.decoded(state, &text);
        }
        if !self.bytes.is_empty() {
            let mut bytes = std::mem::take(&mut self.bytes);
            let text = state.encoding.decode_without_bom_handling(&bytes).0;
            self.decoded(state, &text);
            bytes.clear();
            self.bytes = bytes;
        }
    }

    fn decoded(&mut self, state: RtfState, text: &str) {
        if state.list_label {
            self.list_marker.push_str(&ammonia::clean_text(text.trim()));
        } else {
            self.begin_inline(state);
            styled(&mut self.html, text, state.style);
        }
    }

    fn begin_inline(&mut self, state: RtfState) {
        if self.paragraph_start == self.html.len() {
            if state.in_table || self.table.row_open {
                self.open_cell();
            } else if self.table.open {
                self.close_table();
            }
            self.paragraph = state.paragraph;
        }
    }

    fn raw_inline(&mut self, state: RtfState, text: &str) {
        if state.list_label {
            self.list_marker.push_str(&ammonia::clean_text(text.trim()));
        } else {
            self.begin_inline(state);
            self.html.push_str(text);
        }
    }

    fn end_paragraph(&mut self) {
        let mut tag = String::new();
        let list_item = !self.list_marker.is_empty();
        if list_item {
            if !self.list_open {
                tag.push_str("<ul class=\"rtf-list\">");
                self.list_open = true;
            }
            tag.push_str("<li><span class=\"rtf-list-marker\">");
            tag.push_str(&self.list_marker);
            tag.push_str("</span>");
            self.list_marker.clear();
        } else if self.list_open {
            tag.push_str("</ul>");
            self.list_open = false;
        }
        tag.push_str(&self.paragraph.opening_tag());
        if self.paragraph_start == self.html.len() {
            self.html.push_str("<br>");
        }
        self.html.insert_str(self.paragraph_start, &tag);
        self.html.push_str("</div>");
        if list_item {
            self.html.push_str("</li>");
        }
        self.paragraph_start = self.html.len();
    }

    fn close_list(&mut self) {
        if self.list_open {
            self.html.push_str("</ul>");
            self.paragraph_start = self.html.len();
            self.list_open = false;
        }
    }

    fn open_cell(&mut self) {
        if !self.table.open {
            if self.paragraph_start != self.html.len() {
                self.end_paragraph();
            }
            self.close_list();
            self.table.open = true;
        }
        if !self.table.row_open {
            self.table.row_open = true;
        }
        if !self.table.cell_open {
            self.table.cell_start = self.html.len();
            self.table.cell_open = true;
            self.paragraph_start = self.html.len();
        }
        self.table.defining = false;
    }

    fn close_cell(&mut self) {
        if self.table.cell_open {
            if self.paragraph_start != self.html.len() || !self.list_marker.is_empty() {
                self.end_paragraph();
            }
            self.close_list();
            let content = self.html.split_off(self.table.cell_start);
            self.table.buffered_bytes += content.len();
            self.table.row.push(content);
            self.paragraph_start = self.html.len();
            self.table.cell_open = false;
        }
    }

    fn close_row(&mut self) {
        if self.table.row_open {
            self.close_cell();
            self.table.finish_row();
            self.table.row_open = false;
        }
        self.table.begin_definition();
        self.table.defining = false;
    }

    fn close_table(&mut self) {
        if self.table.open {
            self.close_row();
            std::mem::take(&mut self.table).render(&mut self.html);
            self.paragraph_start = self.html.len();
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
    let mut colors = Vec::new();
    let mut color_table = None;
    let mut rgb = [0; 3];
    let mut has_rgb = false;
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
                if color_table == Some(groups.len()) {
                    color_table = None;
                }
                state = groups.pop()?;
                output.skip = 0;
            }
            Token::ControlWord { name, arg } if color_table == Some(groups.len()) => {
                let channel = match name.as_str() {
                    "red" => Some(0),
                    "green" => Some(1),
                    "blue" => Some(2),
                    _ => None,
                };
                if let (Some(channel), Some(value)) =
                    (channel, arg.and_then(|n| u8::try_from(n).ok()))
                {
                    rgb[channel] = value;
                    has_rgb = true;
                }
            }
            Token::Text(bytes) if color_table == Some(groups.len()) => {
                for byte in bytes {
                    if byte == b';' && colors.len() < 1024 {
                        colors.push(has_rgb.then_some(rgb));
                        rgb = [0; 3];
                        has_rgb = false;
                    }
                }
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
                    "colortbl" => {
                        if groups.len() == 2 && colors.is_empty() {
                            color_table = Some(groups.len());
                        }
                        state.ignored = true;
                    }
                    "fonttbl" | "stylesheet" | "info" | "pict" | "object" | "filetbl"
                    | "listtable" | "listoverridetable" | "header" | "footer" => {
                        state.ignored = true
                    }
                    "listtext" | "pntext" if groups.len() > 1 => {
                        state.list_label = true;
                        output.list_marker.clear();
                    }
                    "b" => state.style.bold = enabled,
                    "i" => state.style.italic = enabled,
                    "ul" => state.style.underline = enabled,
                    "ulnone" => state.style.underline = false,
                    "strike" => state.style.strike = enabled,
                    "plain" => state.style = Style::default(),
                    "fs" => state.style.half_points = arg.filter(|size| (8..=144).contains(size)),
                    "cf" | "highlight" | "cb" => {
                        let color = arg
                            .and_then(|n| usize::try_from(n).ok())
                            .and_then(|i| colors.get(i).copied().flatten());
                        if name == "cf" {
                            state.style.foreground = color;
                        } else {
                            state.style.highlight = color;
                        }
                    }
                    "super" => state.style.script = Some("sup"),
                    "sub" => state.style.script = Some("sub"),
                    "nosupersub" => state.style.script = None,
                    "pard" | "ql" | "qc" | "qr" | "qj" | "fi" | "li" | "ri" | "sb" | "sa"
                    | "sl" | "slmult"
                        if state.list_label =>
                    {
                        ()
                    }
                    "pard" | "ql" | "qc" | "qr" | "qj" | "fi" | "li" | "ri" | "sb" | "sa"
                    | "sl" | "slmult" => {
                        let twips = arg.unwrap_or(0).clamp(-14400, 14400);
                        match name.as_str() {
                            "pard" => {
                                state.paragraph = Paragraph::default();
                                state.in_table = false;
                                state.table_level = 1;
                            }
                            "ql" => state.paragraph.alignment = Some("left"),
                            "qc" => state.paragraph.alignment = Some("center"),
                            "qr" => state.paragraph.alignment = Some("right"),
                            "qj" => state.paragraph.alignment = Some("justify"),
                            "fi" => state.paragraph.first_line = twips,
                            "li" => state.paragraph.left = twips,
                            "ri" => state.paragraph.right = twips,
                            "sb" => state.paragraph.before = twips.max(0),
                            "sa" => state.paragraph.after = twips.max(0),
                            "sl" => state.paragraph.line_spacing = twips,
                            _ => state.paragraph.multiple_spacing = arg == Some(1),
                        }
                        output.paragraph = state.paragraph;
                    }
                    "trowd"
                        if !state.list_label
                            && state.table_level <= 1
                            && (!output.table.row_open
                                || (!output.table.cell_open
                                    && output.table.definitions.is_empty())) =>
                    {
                        output.table.begin_definition();
                    }
                    "trleft" if !state.list_label && output.table.defining => {
                        output.table.left_edge = arg.unwrap_or_default();
                    }
                    "tcelld" if !state.list_label && output.table.defining => {
                        output.table.next_cell = CellDefinition::default();
                    }
                    "clmgf" if !state.list_label && output.table.defining => {
                        output.table.next_cell.horizontal = Merge::Start;
                    }
                    "clmrg" if !state.list_label && output.table.defining => {
                        output.table.next_cell.horizontal = Merge::Continue;
                    }
                    "clvmgf" if !state.list_label && output.table.defining => {
                        output.table.next_cell.vertical = Merge::Start;
                    }
                    "clvmrg" if !state.list_label && output.table.defining => {
                        output.table.next_cell.vertical = Merge::Continue;
                    }
                    "cellx"
                        if !state.list_label
                            && output.table.defining
                            && output.table.definitions.len() < 256 =>
                    {
                        if let Some(edge) = arg {
                            let mut definition = std::mem::take(&mut output.table.next_cell);
                            definition.right = edge;
                            output.table.definitions.push(definition);
                        }
                    }
                    "itap" if !state.list_label => state.table_level = arg.unwrap_or(1),
                    "intbl" if !state.list_label => state.in_table = enabled,
                    "cell" if !state.list_label && (state.in_table || output.table.row_open) => {
                        output.open_cell();
                        output.close_cell();
                    }
                    "row" if !state.list_label => {
                        output.close_row();
                        state.in_table = false;
                    }
                    "par" if !state.list_label => {
                        output.begin_inline(state);
                        output.end_paragraph();
                    }
                    "line" | "nestcell" | "nestrow" if !state.list_label => {
                        output.raw_inline(state, "<br>")
                    }
                    "tab" if !state.list_label => output.raw_inline(state, "\t"),
                    "uc" => state.fallback = arg.unwrap_or(1).clamp(0, 16) as usize,
                    "ansicpg" => {
                        if let Some(code) = arg {
                            state.encoding = rtf_encoding(code).unwrap_or(WINDOWS_1252);
                        }
                    }
                    "emdash" => output.raw_inline(state, "—"),
                    "endash" => output.raw_inline(state, "–"),
                    "bullet" => output.raw_inline(state, "•"),
                    "lquote" => output.raw_inline(state, "‘"),
                    "rquote" => output.raw_inline(state, "’"),
                    "ldblquote" => output.raw_inline(state, "“"),
                    "rdblquote" => output.raw_inline(state, "”"),
                    _ => (),
                }
            }
            _ => (),
        }
        if output.html.len() + output.list_marker.len() + output.table.buffered_bytes
            > RTF_OUTPUT_LIMIT
        {
            return None;
        }
    }
    if !groups.is_empty() {
        return None;
    }
    if output.paragraph_start != output.html.len() || !output.list_marker.is_empty() {
        output.begin_inline(state);
        output.end_paragraph();
    }
    output.close_table();
    output.close_list();
    (output.html.len() <= RTF_OUTPUT_LIMIT)
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
    let span =
        style.half_points.is_some() || style.foreground.is_some() || style.highlight.is_some();
    if span {
        html.push_str("<span style=\"");
        if let Some(size) = style.half_points {
            let _ = write!(html, "font-size:{}pt;", f64::from(size) / 2.0);
        }
        for (property, color) in [
            ("color", style.foreground),
            ("background-color", style.highlight),
        ] {
            if let Some([r, g, b]) = color {
                let _ = write!(html, "{property}:#{r:02x}{g:02x}{b:02x};");
            }
        }
        html.push_str("\">");
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
    if let Some(tag) = style.script {
        let _ = write!(html, "<{tag}>");
    }
    html.push_str(&ammonia::clean_text(text));
    if let Some(tag) = style.script {
        let _ = write!(html, "</{tag}>");
    }
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
    if span {
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
                && html.contains("</div><div>")
                && html.contains("😀")
                && html.contains("&lt;b&gt;")
        );
        for forbidden in ["Arial", "payload", "?"] {
            assert!(!html.contains(forbidden));
        }
    }

    #[test]
    fn rtf_colors_and_baseline_follow_the_group_scope() {
        let rtf = br#"{\rtf1{\colortbl;\red255\green0\blue16;\red250\green240\blue50;}plain {\cf1\highlight2\super <2>} {\sub 3} final}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(html.contains(
            "<span style=\"color:#ff0010;background-color:#faf032;\"><sup>&lt;2&gt;</sup></span>"
        ));
        assert!(html.contains("<sub>3</sub>&#32;final"));
    }

    #[test]
    fn rtf_lays_out_paragraphs_and_resets_their_properties() {
        let rtf = br#"{\rtf1\qc\li720\fi-200\sb120\sa240 Title{\b !}\line More\par\pard Plain\par\par {\qr Right}\par Last}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(html.contains("<div style=\"text-align:center;text-indent:-10pt;margin-left:36pt;padding-top:6pt;padding-bottom:12pt;\">Title<b>!</b><br>More</div>"));
        assert!(html.contains("<div>Plain</div><div><br></div><div style=\"text-align:right;\">Right</div><div>Last</div>"));
    }

    #[test]
    fn rtf_distinguishes_multiple_exact_and_minimum_line_spacing() {
        let rtf = br#"{\rtf1\sl360\slmult1 Multiple\par\pard\sl-300\slmult0 Fixed\par\pard\sl480 Minimum\par\pard Normal}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(html.contains("<div style=\"line-height:1.5;\">Multiple</div>"));
        assert!(html.contains("<div style=\"line-height:15pt;\">Fixed</div>"));
        assert!(html.contains(
            "<div style=\"line-height:max(1.2em,24pt);\">Minimum</div><div>Normal</div>"
        ));
    }

    #[test]
    fn rtf_renders_table_rows_and_cells_as_safe_html() {
        let rtf = br#"{\rtf1 Before\par\trowd\trleft-100\cellx1900\cellx3900
\pard\intbl A {\b bold}\cell\pard\intbl <script>\par second paragraph\cell\row
\trowd\cellx2000\cellx4000\intbl C\cell\intbl\cell\row\pard After}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(html.contains("<div>Before</div><table class=\"rtf-table\"><tbody><tr>"));
        assert!(html.contains("<td style=\"width:100pt;\"><div>A&#32;<b>bold</b></div></td>"));
        assert!(html.contains("<div>&lt;script&gt;</div><div>second&#32;paragraph</div>"));
        assert!(html.contains("</tr><tr>"));
        assert!(html.contains(
            "</td><td style=\"width:100pt;\"></td></tr></tbody></table><div>After</div>"
        ));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn rtf_uses_explicit_list_markers_and_keeps_other_destinations_hidden() {
        let rtf = br#"{\rtf1{\*\listtable hidden}
{\listtext\bullet\tab}First\par{\pntext 2.\tab}Second <img>\par\pard Outside}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(html.contains("<ul class=\"rtf-list\"><li><span class=\"rtf-list-marker\">•</span><div>First</div></li>"));
        assert!(html.contains("<li><span class=\"rtf-list-marker\">2.</span><div>Second&#32;&lt;img&gt;</div></li></ul><div>Outside</div>"));
        assert!(!html.contains("hidden"));
    }

    #[test]
    fn rtf_combines_horizontal_and_vertical_merges_without_losing_contents() {
        let rtf = br#"{\rtf1
\trowd\clmgf\clvmgf\cellx1000\clmrg\cellx2000\cellx3000
\pard\intbl A\cell B\cell First\cell\row
\trowd\clmgf\clvmrg\cellx1000\clmrg\cellx2000\cellx3000
\pard\intbl <C>\cell\cell Second\cell\row
\trowd\clvmrg\cellx1800\cellx3000
\pard\intbl No match\cell Third\cell\row}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(html.contains("<td colspan=\"2\" rowspan=\"2\" style=\"width:100pt;\"><div>A</div><div>B</div><div>&lt;C&gt;</div></td>"));
        assert!(html.contains("<tr><td style=\"width:50pt;\"><div>Second</div></td></tr>"));
        assert!(html.contains("<tr><td style=\"width:90pt;\"><div>No&#32;match</div></td>"));
    }

    #[test]
    fn rtf_merges_cells_when_row_properties_follow_their_contents() {
        let rtf = br#"{\rtf1\pard\intbl One\cell Two\cell
{\trowd\clmgf\cellx1000\clmrg\cellx2000}\row}"#;
        let html = rtf_html(rtf).unwrap();
        assert!(html.contains(
            "<td colspan=\"2\" style=\"width:100pt;\"><div>One</div><div>Two</div></td>"
        ));
    }
}
