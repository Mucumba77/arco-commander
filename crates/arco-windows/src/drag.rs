use crate::shell_items::{Ole, item_array};
use std::io;
use std::path::PathBuf;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{IBindCtx, IDataObject};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, IDropSource};
use windows::Win32::UI::Shell::{BHID_DataObject, SHDoDragDrop};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;

pub fn drag_copy(paths: &[PathBuf]) -> io::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let _ole = Ole::new()?;
    let items = item_array(paths)?;
    let data: IDataObject = unsafe { items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) }
        .map_err(io::Error::other)?;
    let window = HWND(unsafe { GetActiveWindow() });
    // The Shell supplies IDropSource. A copy effect leaves source deletion to nobody.
    unsafe { SHDoDragDrop(Some(window), &data, None::<&IDropSource>, DROPEFFECT_COPY) }
        .map_err(io::Error::other)?;
    Ok(())
}
