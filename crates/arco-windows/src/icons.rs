use std::{io, os::windows::ffi::OsStrExt, path::Path, ptr::null_mut};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GdiFlush, SelectObject,
};
use windows_sys::Win32::UI::Shell::ExtractIconExW;
use windows_sys::Win32::UI::WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON};

const SIZE: i32 = 32;
const BYTES: usize = SIZE as usize * SIZE as usize * 4;

pub fn resource_icon(path: &Path, index: i32) -> io::Result<Vec<u8>> {
    let mut path: Vec<_> = path.as_os_str().encode_wide().collect();
    if path.is_empty() || path.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    path.push(0);
    let mut icon = null_mut();
    // ExtractIconEx reads the icon resources of the specified file.
    let count = unsafe { ExtractIconExW(path.as_ptr(), index, &mut icon, null_mut(), 1) };
    if count != 1 || icon.is_null() {
        if !icon.is_null() {
            unsafe { DestroyIcon(icon) };
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Symbolressource fehlt.",
        ));
    }
    let pixels = draw(icon);
    unsafe { DestroyIcon(icon) };
    pixels
}

fn draw(icon: HICON) -> io::Result<Vec<u8>> {
    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = size_of_val(&info.bmiHeader) as u32;
    info.bmiHeader.biWidth = SIZE;
    info.bmiHeader.biHeight = -SIZE;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    let dc = unsafe { CreateCompatibleDC(null_mut()) };
    if dc.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut bits = null_mut();
    let bitmap = unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
    if bitmap.is_null() || bits.is_null() {
        let error = io::Error::last_os_error();
        if !bitmap.is_null() {
            unsafe { DeleteObject(bitmap) };
        }
        unsafe { DeleteDC(dc) };
        return Err(error);
    }
    let previous = unsafe { SelectObject(dc, bitmap) };
    let result = if previous.is_null() {
        Err(io::Error::last_os_error())
    } else {
        let render = |background: u8| -> io::Result<Vec<u8>> {
            // Flush GDI before accessing the DIB section's 32-bit, top-down pixels.
            unsafe {
                GdiFlush();
                std::ptr::write_bytes(bits.cast::<u8>(), background, BYTES);
                if DrawIconEx(dc, 0, 0, icon, SIZE, SIZE, 0, null_mut(), DI_NORMAL) == 0 {
                    return Err(io::Error::last_os_error());
                }
                GdiFlush();
                Ok(std::slice::from_raw_parts(bits.cast::<u8>(), BYTES).to_vec())
            }
        };
        let result = render(0).and_then(|dark| {
            let light = render(255)?;
            let mut rgba = Vec::with_capacity(BYTES);
            // Two backgrounds recover straight alpha for both alpha and mask-based icons.
            for (dark, light) in dark.chunks_exact(4).zip(light.chunks_exact(4)) {
                let transmitted = (0..3)
                    .map(|i| light[i].saturating_sub(dark[i]))
                    .max()
                    .unwrap();
                let alpha = 255 - transmitted;
                for index in [2, 1, 0] {
                    let channel = if alpha == 0 {
                        0
                    } else {
                        (u32::from(dark[index]) * 255 / u32::from(alpha)).min(255) as u8
                    };
                    rgba.push(channel);
                }
                rgba.push(alpha);
            }
            Ok(rgba)
        });
        unsafe { SelectObject(dc, previous) };
        result
    };
    unsafe {
        DeleteObject(bitmap);
        DeleteDC(dc);
    }
    result
}

#[cfg(test)]
#[test]
fn system_icon_resource_has_visible_and_transparent_pixels() {
    let library = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("shell32.dll");
    let pixels = resource_icon(&library, 0).unwrap();
    assert_eq!(pixels.len(), BYTES);
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
}
