use std::{io, ptr::null_mut};
use windows_sys::Win32::Foundation::{GlobalFree, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, DeleteDC, DeleteObject, GetDeviceCaps, GetTextExtentExPointW, GetTextMetricsW,
    HDC, HFONT, HGDIOBJ, HORZRES, LOGPIXELSX, LOGPIXELSY, SelectObject, TEXTMETRICW, TextOutW,
    VERTRES,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PD_HIDEPRINTTOFILE, PD_NOPAGENUMS, PD_NOSELECTION, PD_RETURNDC,
    PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW, PrintDlgW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;

struct Printer(PRINTDLGW);

impl Drop for Printer {
    fn drop(&mut self) {
        unsafe {
            if !self.0.hDC.is_null() {
                DeleteDC(self.0.hDC);
            }
            if !self.0.hDevMode.is_null() {
                GlobalFree(self.0.hDevMode);
            }
            if !self.0.hDevNames.is_null() {
                GlobalFree(self.0.hDevNames);
            }
        }
    }
}

struct Font {
    dc: HDC,
    created: HFONT,
    previous: HGDIOBJ,
}

impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.created);
        }
    }
}

pub fn print_text(title: &str, body: &str) -> io::Result<bool> {
    let mut printer = Printer(PRINTDLGW {
        lStructSize: std::mem::size_of::<PRINTDLGW>() as u32,
        hwndOwner: unsafe { GetActiveWindow() },
        Flags: PD_RETURNDC
            | PD_USEDEVMODECOPIESANDCOLLATE
            | PD_NOPAGENUMS
            | PD_NOSELECTION
            | PD_HIDEPRINTTOFILE,
        nCopies: 1,
        ..Default::default()
    });
    if unsafe { PrintDlgW(&mut printer.0) } == 0 {
        let code = unsafe { CommDlgExtendedError() };
        return if code == 0 {
            Ok(false)
        } else {
            Err(io::Error::other(format!("Druckdialog · Fehler {code:#x}")))
        };
    }
    let dc = printer.0.hDC;
    if dc.is_null() {
        return Err(io::Error::other(
            "Der Druckdialog hat keinen Drucker bereitgestellt.",
        ));
    }
    let dpi_x = unsafe { GetDeviceCaps(dc, LOGPIXELSX as i32) };
    let dpi_y = unsafe { GetDeviceCaps(dc, LOGPIXELSY as i32) };
    let left = (dpi_x / 3).max(1);
    let top = (dpi_y / 3).max(1);
    let width = unsafe { GetDeviceCaps(dc, HORZRES as i32) } - 2 * left;
    let bottom = unsafe { GetDeviceCaps(dc, VERTRES as i32) } - top;
    let face: Vec<_> = "Consolas".encode_utf16().chain([0]).collect();
    let font = unsafe {
        CreateFontW(
            -(10 * dpi_y / 72).max(1),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
            0,
            face.as_ptr(),
        )
    };
    if font.is_null() {
        return Err(io::Error::other(
            "Die Druckschrift konnte nicht erstellt werden.",
        ));
    }
    let previous = unsafe { SelectObject(dc, font) };
    if previous.is_null() || previous as isize == -1 {
        unsafe { DeleteObject(font) };
        return Err(io::Error::other(
            "Die Druckschrift konnte nicht gewählt werden.",
        ));
    }
    let _font = Font {
        dc,
        created: font,
        previous,
    };
    let mut metrics = TEXTMETRICW::default();
    if unsafe { GetTextMetricsW(dc, &mut metrics) } == 0 {
        return Err(io::Error::other(
            "Die Schriftmaße konnten nicht gelesen werden.",
        ));
    }
    let height = (metrics.tmHeight + metrics.tmExternalLeading).max(1);
    if width < 1 || bottom - top < height {
        return Err(io::Error::other("Der Druckbereich ist zu klein."));
    }
    let title: Vec<_> = title
        .encode_utf16()
        .filter(|&character| character != 0)
        .chain([0])
        .collect();
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: title.as_ptr(),
        ..Default::default()
    };
    if unsafe { StartDocW(dc, &info) } <= 0 {
        return Err(io::Error::other(
            "Der Druckauftrag konnte nicht gestartet werden.",
        ));
    }
    if let Err(error) = draw_pages(dc, body, left, top, bottom, width, height) {
        unsafe { AbortDoc(dc) };
        return Err(error);
    }
    if unsafe { EndDoc(dc) } <= 0 {
        return Err(io::Error::other(
            "Der Druckauftrag konnte nicht abgeschlossen werden.",
        ));
    }
    Ok(true)
}

fn draw_pages(
    dc: HDC,
    body: &str,
    left: i32,
    top: i32,
    bottom: i32,
    width: i32,
    height: i32,
) -> io::Result<()> {
    if unsafe { StartPage(dc) } <= 0 {
        return Err(io::Error::other(
            "Druckseite konnte nicht gestartet werden.",
        ));
    }
    let mut y = top;
    for line in body.lines() {
        let line: Vec<_> = line.replace('\t', "    ").encode_utf16().collect();
        let mut remaining = line.as_slice();
        loop {
            if y + height > bottom {
                if unsafe { EndPage(dc) } <= 0 || unsafe { StartPage(dc) } <= 0 {
                    return Err(io::Error::other("Druckseitenwechsel fehlgeschlagen."));
                }
                y = top;
            }
            if remaining.is_empty() {
                y += height;
                break;
            }
            let mut fit = 0;
            let mut size = SIZE::default();
            let units = remaining.len().min(32767) as i32;
            if unsafe {
                GetTextExtentExPointW(
                    dc,
                    remaining.as_ptr(),
                    units,
                    width,
                    &mut fit,
                    null_mut(),
                    &mut size,
                )
            } == 0
            {
                return Err(io::Error::other(
                    "Druckzeile konnte nicht vermessen werden.",
                ));
            }
            let mut end = fit.clamp(1, units) as usize;
            if remaining
                .get(end)
                .is_some_and(|&unit| (0xdc00..=0xdfff).contains(&unit))
            {
                if end == 1 {
                    end += 1;
                } else {
                    end -= 1;
                }
            }
            if unsafe { TextOutW(dc, left, y, remaining.as_ptr(), end as i32) } == 0 {
                return Err(io::Error::other(
                    "Druckzeile konnte nicht ausgegeben werden.",
                ));
            }
            y += height;
            remaining = &remaining[end..];
            if remaining.is_empty() {
                break;
            }
        }
    }
    if unsafe { EndPage(dc) } <= 0 {
        Err(io::Error::other(
            "Druckseite konnte nicht abgeschlossen werden.",
        ))
    } else {
        Ok(())
    }
}
