use arco_core::files::{self, Entry, Kind};
use arco_core::tools::{self, Preview, PreviewMode};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const IMAGE_LIMIT: u64 = 16 * 1024 * 1024;

mod rich;
pub(crate) use rich::Rich;

pub(crate) struct ThumbnailRequest {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

impl ThumbnailRequest {
    pub(crate) fn for_entry(entry: &Entry) -> Option<Self> {
        let size = entry.size?;
        let modified = entry.modified?;
        (entry.kind == Kind::File && size <= IMAGE_LIMIT && image_extension(&entry.path)).then(
            || Self {
                path: entry.path.clone(),
                size,
                modified,
            },
        )
    }

    pub(crate) fn load(&self) -> Option<image::RgbaImage> {
        let before = files::regular_metadata(&self.path).ok()?;
        if !before.is_file()
            || before.len() != self.size
            || before.modified().ok()? != self.modified
        {
            return None;
        }
        let image = decode_image(&self.path, 144, 88).ok()?;
        files::ensure_unchanged(&self.path, &before).ok()?;
        Some(image)
    }
}

pub enum Content {
    Page(PreviewMode, Preview),
    Image(image::RgbaImage),
    Rich(Rich),
}

pub fn read(path: &Path) -> io::Result<Content> {
    let metadata = arco_core::files::regular_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::other("Wähle eine reguläre Datei."));
    }
    if let Some(rich) = rich::read(path, &metadata)? {
        return Ok(Content::Rich(rich));
    }
    read_raw_with_metadata(path, &metadata)
}

pub(crate) fn read_raw(path: &Path) -> io::Result<Content> {
    let metadata = files::regular_metadata(path)?;
    read_raw_with_metadata(path, &metadata)
}

fn read_raw_with_metadata(path: &Path, metadata: &std::fs::Metadata) -> io::Result<Content> {
    if image_extension(path)
        && metadata.len() <= IMAGE_LIMIT
        && let Ok(image) = decode_image(path, 1280, 1280)
    {
        return Ok(Content::Image(image));
    }
    let page = tools::preview(path, 0, PreviewMode::Text)?;
    if page.text.chars().take(4096).any(|character| {
        character == '\0'
            || (character.is_ascii_control() && !matches!(character, '\n' | '\r' | '\t' | '\x0c'))
    }) {
        let mode = PreviewMode::Hex;
        Ok(Content::Page(mode, tools::preview(path, 0, mode)?))
    } else {
        Ok(Content::Page(PreviewMode::Text, page))
    }
}

fn image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "bmp" | "gif" | "jpg" | "jpeg" | "png" | "webp"
            )
        })
}

pub(crate) fn decode_image(
    path: &Path,
    width: u32,
    height: u32,
) -> image::ImageResult<image::RgbaImage> {
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?.thumbnail(width, height).into_rgba8())
}

pub fn to_slint(image: &image::RgbaImage) -> slint::Image {
    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_is_scaled_and_cannot_reuse_a_stale_directory_entry() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("image.png");
        image::RgbaImage::from_pixel(288, 144, image::Rgba([3, 47, 65, 255]))
            .save(&path)
            .unwrap();
        let entry = files::list(fixture.path()).unwrap().entries.remove(0);
        let request = ThumbnailRequest::for_entry(&entry).unwrap();
        let image = request.load().unwrap();
        assert_eq!(image.dimensions(), (144, 72));
        assert_eq!(image.get_pixel(0, 0).0, [3, 47, 65, 255]);
        std::fs::write(&path, b"replaced").unwrap();
        assert!(request.load().is_none());
    }
}
