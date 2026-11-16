use super::{WdxKey, WdxValue};
use crate::{files, io_progress};
use lofty::{
    config::ParseOptions,
    file::TaggedFileExt,
    probe::Probe,
    tag::{Accessor, ItemKey, TagType},
};
use std::{
    fs::Metadata,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::AtomicBool,
};

pub(super) fn is_field(field: &str) -> bool {
    matches!(
        field,
        "tag present"
            | "title"
            | "artist"
            | "album"
            | "release year"
            | "description"
            | "track nr"
            | "genre"
    )
}

struct Budget<R> {
    inner: R,
    remaining: usize,
}
impl<R: Read> Read for Budget<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(io::Error::other(
                "Audio-Metadaten überschreiten das Leselimit von 64 MiB.",
            ));
        }
        let limit = buffer.len().min(self.remaining);
        let read = self.inner.read(&mut buffer[..limit])?;
        self.remaining -= read;
        Ok(read)
    }
}
impl<R: Seek> Seek for Budget<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

pub(super) fn field(
    path: &Path,
    metadata: &Metadata,
    field: &str,
    cancel: &AtomicBool,
) -> io::Result<Option<WdxValue>> {
    files::check_cancel(cancel)?;
    files::ensure_unchanged(path, metadata)?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let input = io_progress::ObservedFile::open(path, cancel, &|_| {})?;
    let reader = Budget {
        inner: input,
        remaining: 64 * 1024 * 1024,
    };
    let probe = Probe::new(reader)
        .guess_file_type()
        .map_err(|error| io::Error::other(error.to_string()))?;
    if probe.file_type().is_none() {
        return Ok((field == "tag present").then(|| WdxValue {
            display: "false".into(),
            key: WdxKey::Boolean(false),
        }));
    }
    let result = probe
        .options(
            ParseOptions::new()
                .read_properties(false)
                .read_cover_art(false),
        )
        .read();
    files::check_cancel(cancel)?;
    files::ensure_unchanged(path, metadata)?;
    let file = result.map_err(|error| io::Error::other(error.to_string()))?;
    if field == "tag present" {
        let present = file.tags().iter().any(|tag| tag.item_count() > 0);
        return Ok(Some(WdxValue {
            display: present.to_string(),
            key: WdxKey::Boolean(present),
        }));
    }
    let primary = file.primary_tag_type();
    let mut tags = file.tags().iter().collect::<Vec<_>>();
    tags.sort_by_key(|tag| {
        if tag.tag_type() == primary {
            0
        } else if tag.tag_type() == TagType::Id3v1 {
            2
        } else {
            1
        }
    });
    let v2 = tags.iter().any(|tag| tag.tag_type() == TagType::Id3v2);
    for tag in tags {
        let text = match field {
            "title" => tag.title(),
            "artist" => tag.artist(),
            "album" => tag.album(),
            "description" => tag
                .get_string(ItemKey::Description)
                .or_else(|| tag.get_string(ItemKey::Comment))
                .map(std::borrow::Cow::Borrowed)
                .or_else(|| {
                    if tag.tag_type() != TagType::Id3v2 {
                        return None;
                    }
                    let native = lofty::id3::v2::Id3v2Tag::from((*tag).clone());
                    [
                        "comment",
                        "COMMENT",
                        "Comment",
                        "description",
                        "DESCRIPTION",
                        "Description",
                    ]
                    .into_iter()
                    .find_map(|name| {
                        native
                            .get_user_text(name)
                            .map(|value| std::borrow::Cow::Owned(value.to_owned()))
                    })
                }),
            "genre" => tag.genre(),
            "release year" => {
                if let Some(date) = tag.date() {
                    return Ok(Some(WdxValue {
                        display: date.year.to_string(),
                        key: WdxKey::Integer(i64::from(date.year)),
                    }));
                }
                continue;
            }
            "track nr" => {
                if let Some(track) = tag.track() {
                    return Ok(Some(WdxValue {
                        display: track.to_string(),
                        key: WdxKey::Integer(i64::from(track)),
                    }));
                }
                continue;
            }
            _ => None,
        };
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            // TC treats ID3v1 genre zero as unset when filling an incomplete v2 tag.
            if field == "genre" && v2 && tag.tag_type() == TagType::Id3v1 && text == "Blues" {
                continue;
            }
            return Ok(Some(WdxValue {
                key: WdxKey::Text(text.to_lowercase()),
                display: text.into_owned(),
            }));
        }
    }
    Ok(None)
}
