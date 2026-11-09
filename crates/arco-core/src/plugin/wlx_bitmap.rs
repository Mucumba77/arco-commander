use super::{Interface, Operation, Row, inspect_host};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const WLX_BITMAP_WIDTH: u32 = 144;
pub const WLX_BITMAP_HEIGHT: u32 = 88;
pub const WLX_BITMAP_BATCH: usize = 8;

pub struct WlxBitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl WlxBitmap {
    pub fn rgba_size(width: u32, height: u32) -> Option<usize> {
        ((1..=WLX_BITMAP_WIDTH).contains(&width) && (1..=WLX_BITMAP_HEIGHT).contains(&height))
            .then(|| (width * height * 4) as usize)
    }

    pub fn into_row(self, index: usize) -> Row {
        Row {
            name: index.to_string(),
            value: format!(
                "{}:{}:{}",
                self.width,
                self.height,
                STANDARD.encode(self.rgba)
            ),
            ..Default::default()
        }
    }

    fn from_value(value: &str) -> Option<Self> {
        let mut fields = value.splitn(3, ':');
        let width = fields.next()?.parse().ok()?;
        let height = fields.next()?.parse().ok()?;
        let size = Self::rgba_size(width, height)?;
        let encoded = fields.next()?;
        if encoded.len() != size.div_ceil(3) * 4 {
            return None;
        }
        let rgba = STANDARD.decode(encoded).ok()?;
        (rgba.len() == size).then_some(Self {
            width,
            height,
            rgba,
        })
    }
}

pub fn wlx_thumbnails(
    plugin: &Path,
    files: Vec<PathBuf>,
    cancel: &AtomicBool,
) -> io::Result<Vec<(usize, WlxBitmap)>> {
    let count = files.len();
    if !(1..=WLX_BITMAP_BATCH).contains(&count) || files.iter().any(|file| !file.is_absolute()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ungültige WLX-Miniaturanfrage.",
        ));
    }
    let rows = inspect_host(
        Interface::Wlx,
        plugin,
        Path::new(""),
        Operation::WlxThumbnails { files },
        cancel,
        |_| (),
    )?;
    let mut seen = [false; WLX_BITMAP_BATCH];
    rows.into_iter()
        .map(|row| {
            let index = row
                .name
                .parse::<usize>()
                .ok()
                .filter(|&index| index < count && !seen[index]);
            let parsed = index.zip(WlxBitmap::from_value(&row.value));
            let (index, image) = parsed.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "Ungültiges WLX-Miniaturbild.")
            })?;
            seen[index] = true;
            Ok((index, image))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wlx_bitmap_decoding_checks_dimensions_and_exact_pixel_count() {
        let valid = WlxBitmap {
            width: 1,
            height: 1,
            rgba: vec![2, 9, 3, 255],
        }
        .into_row(0);
        assert_eq!(
            WlxBitmap::from_value(&valid.value).unwrap().rgba,
            [2, 9, 3, 255]
        );
        for invalid in [
            "0:1:AAAAAA==",
            "145:1:AAAAAA==",
            "1:1:AAAA",
            "1:1:!!!!!!!!",
            "1:1:AAAAAAAA",
        ] {
            assert!(WlxBitmap::from_value(invalid).is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the ANSI and Unicode WLX test DLLs in ARCO_TEST_WLX_BITMAP_X86 and ARCO_TEST_WLX_BITMAP_X64"]
    fn wlx_thumbnail_batch_calls_both_native_exports_and_rejects_oversize() {
        let fixture = tempfile::tempdir().unwrap();
        for (architecture, name) in [("X86", "preview.txt"), ("X64", "Grüße-𐐀.txt")] {
            let plugin = PathBuf::from(
                std::env::var_os(format!("ARCO_TEST_WLX_BITMAP_{architecture}")).unwrap(),
            );
            let valid = fixture.path().join(name);
            let mut contents = vec![0; 9000];
            contents[0] = b'A';
            contents[8191] = b'Z';
            std::fs::write(&valid, &contents).unwrap();
            let oversized = fixture.path().join("oversized.txt");
            contents[0] = b'O';
            std::fs::write(&oversized, &contents).unwrap();
            let images =
                wlx_thumbnails(&plugin, vec![oversized, valid], &AtomicBool::new(false)).unwrap();
            assert_eq!(images.len(), 1);
            let (index, image) = &images[0];
            assert_eq!(*index, 1);
            assert_eq!((image.width, image.height), (2, 2));
            assert_eq!(&image.rgba[0..4], [224, 32, 16, 255]);
            assert_eq!(&image.rgba[8..12], [17, 38, 61, 255]);
        }
    }
}
