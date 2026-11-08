use arco_core::tools::{self, Preview, PreviewMode};
use std::io;
use std::path::Path;

pub enum Content {
    Page(PreviewMode, Preview),
    Image(image::RgbaImage),
}

pub fn read(path: &Path) -> io::Result<Content> {
    let metadata = arco_core::files::regular_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::other("Wähle eine reguläre Datei."));
    }
    let is_image = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "bmp" | "gif" | "jpg" | "jpeg" | "png" | "webp"
            )
        });
    if is_image
        && metadata.len() <= 16 * 1024 * 1024
        && let Ok(image) = decode_image(path)
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

fn decode_image(path: &Path) -> image::ImageResult<image::RgbaImage> {
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?.thumbnail(1280, 1280).into_rgba8())
}

pub fn to_slint(image: &image::RgbaImage) -> slint::Image {
    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}
