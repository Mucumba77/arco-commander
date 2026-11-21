use super::{Entry, ListFormat, Listing, Session, Url, check_segment};
use encoding_rs::Encoding;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, CharacterTokens, EndTag, StartTag, TagToken, Token, TokenSink, TokenSinkResult,
    Tokenizer,
};
use percent_encoding::percent_decode_str;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::AtomicBool;

pub(super) fn read_only() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "FTP über HTTP-Forward unterstützt Verzeichnislisten und Downloads per GET.",
    )
}

impl Session {
    pub(super) fn forward_listing_html(
        &self,
        directory: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<String> {
        let response = self.http(directory, "GET")?.send(cancel)?.expect(&[200])?;
        let options = &self.connection.options;
        let mut encoding = options.encoding;
        if let Some(content_type) = response.headers.get("content-type") {
            let mut parts = content_type.split(';');
            if !parts.next().is_some_and(|kind| {
                kind.trim().eq_ignore_ascii_case("text/html")
                    || kind.trim().eq_ignore_ascii_case("application/xhtml+xml")
            }) {
                return Err(invalid_listing());
            }
            if let Some(label) =
                parts
                    .filter_map(|part| part.split_once('='))
                    .find_map(|(key, value)| {
                        key.trim()
                            .eq_ignore_ascii_case("charset")
                            .then_some(value.trim().trim_matches('"'))
                    })
            {
                encoding = Encoding::for_label(label.as_bytes()).ok_or_else(invalid_listing)?;
            }
        }
        encoding
            .decode_without_bom_handling_and_without_replacement(&response.body)
            .map(|html| html.into_owned())
            .ok_or_else(invalid_listing)
    }

    pub(super) fn list_forward(&self, directory: &Url, cancel: &AtomicBool) -> io::Result<Listing> {
        let html = self.forward_listing_html(directory, cancel)?;
        super::check_cancel(cancel)?;
        let options = &self.connection.options;
        let address = options.encode_path(directory)?;
        let entries = parse_listing(&html, &address, options.encoding)?;
        Ok(Listing {
            entries,
            format: ListFormat::HttpForward,
        })
    }
}

fn invalid_listing() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Der HTTP-Proxy lieferte keine auswertbare FTP-Verzeichnisliste.",
    )
}

#[derive(Default)]
struct ListingData {
    containers: usize,
    saw_container: bool,
    conflict: bool,
    prefix: String,
    entries: BTreeMap<String, Entry>,
}

struct Links<'a> {
    directory: &'a Url,
    encoding: &'static Encoding,
    data: RefCell<ListingData>,
}

impl TokenSink for Links<'_> {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut data = self.data.borrow_mut();
        match token {
            TagToken(tag) if tag.kind == StartTag => match tag.name.as_ref() {
                "script" => return TokenSinkResult::RawData(RawKind::ScriptData),
                "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                    return TokenSinkResult::RawData(RawKind::Rawtext);
                }
                "title" | "textarea" => return TokenSinkResult::RawData(RawKind::Rcdata),
                "plaintext" => return TokenSinkResult::Plaintext,
                "pre" | "table" => {
                    data.containers += 1;
                    data.saw_container = true;
                    data.prefix.clear();
                }
                "tr" | "br" => data.prefix.clear(),
                "a" if data.containers > 0 => {
                    if let Some(href) = tag.attrs.iter().find(|attr| &*attr.name.local == "href")
                        && let Some(entry) =
                            child_entry(self.directory, self.encoding, &href.value, &data.prefix)
                    {
                        if let Some(previous) = data.entries.get(&entry.name)
                            && previous != &entry
                        {
                            data.conflict = true;
                        }
                        data.entries.insert(entry.name.clone(), entry);
                    }
                }
                _ => (),
            },
            TagToken(tag) if tag.kind == EndTag && matches!(tag.name.as_ref(), "pre" | "table") => {
                data.containers = data.containers.saturating_sub(1);
                data.prefix.clear();
            }
            CharacterTokens(value) if data.containers > 0 => {
                if let Some((_, tail)) = value.rsplit_once('\n') {
                    data.prefix.clear();
                    data.prefix.push_str(tail);
                } else {
                    data.prefix.push_str(&value);
                }
            }
            _ => (),
        }
        TokenSinkResult::Continue
    }
}

fn parse_listing(
    html: &str,
    directory: &Url,
    encoding: &'static Encoding,
) -> io::Result<Vec<Entry>> {
    let tokenizer = Tokenizer::new(
        Links {
            directory,
            encoding,
            data: RefCell::default(),
        },
        Default::default(),
    );
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let data = tokenizer.sink.data.into_inner();
    if !data.saw_container || data.conflict {
        return Err(invalid_listing());
    }
    Ok(data.entries.into_values().collect())
}

fn child_entry(
    directory: &Url,
    encoding: &'static Encoding,
    href: &str,
    prefix: &str,
) -> Option<Entry> {
    let url = directory.join(href.trim()).ok()?;
    if url.scheme() != "ftp"
        || url.host_str() != directory.host_str()
        || url.port() != directory.port()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let mut path = url.path();
    let mut is_directory = path.ends_with('/');
    if let Some((name, qualifier)) = path.rsplit_once(";type=")
        && matches!(qualifier.to_ascii_lowercase().as_str(), "a" | "i" | "d")
    {
        is_directory |= qualifier.eq_ignore_ascii_case("d") || name.ends_with('/');
        path = name;
    }
    let (parent, name) = path.trim_end_matches('/').rsplit_once('/')?;
    let parent = percent_decode_str(parent).collect::<Vec<_>>();
    let expected = percent_decode_str(directory.path().trim_end_matches('/')).collect::<Vec<_>>();
    if parent != expected {
        return None;
    }
    let name_bytes = percent_decode_str(name).collect::<Vec<_>>();
    let name = encoding
        .decode_without_bom_handling_and_without_replacement(&name_bytes)?
        .into_owned();
    check_segment(&name).ok()?;
    let prefix = prefix.trim_start();
    if prefix.starts_with('l') && prefix.split_whitespace().next()?.len() >= 10 {
        return None;
    }
    // Apache emits the original Unix or DOS LIST fields before each link.
    let line = format!("{} {name}", prefix.replace("[DIR]", "<DIR>"));
    let details = super::listing::parse_list(line.as_bytes())
        .and_then(|mut entries| entries.pop())
        .filter(|entry| entry.name == name);
    let directory = is_directory
        || details
            .as_ref()
            .is_some_and(|entry| entry.directory == Some(true));
    Some(Entry {
        name,
        directory: Some(directory),
        symlink: false,
        size: details.and_then(|entry| entry.size).filter(|_| !directory),
        modified: None,
        unique: None,
    })
}
