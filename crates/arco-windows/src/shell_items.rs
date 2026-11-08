use std::io;
use std::path::PathBuf;
use std::ptr::null_mut;
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IShellItemArray, SHCreateShellItemArrayFromIDLists, SHParseDisplayName,
};
use windows::core::PCWSTR;

pub(crate) fn item_array(paths: &[PathBuf]) -> io::Result<IShellItemArray> {
    let ids = paths
        .iter()
        .map(|path| {
            let name = crate::platform::wide(path)?;
            let mut id = Pidl(null_mut());
            // The Shell allocates the item ID, which remains owned by this guard.
            unsafe {
                SHParseDisplayName(PCWSTR(name.as_ptr()), None::<&IBindCtx>, &mut id.0, 0, None)
            }
            .map_err(io::Error::other)?;
            Ok(id)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let pointers: Vec<_> = ids.iter().map(|id| id.0.cast_const()).collect();
    // The array owns copies of the item IDs.
    unsafe { SHCreateShellItemArrayFromIDLists(&pointers) }.map_err(io::Error::other)
}

pub(crate) struct Ole;

impl Ole {
    pub(crate) fn new() -> io::Result<Self> {
        unsafe { OleInitialize(None) }.map_err(io::Error::other)?;
        Ok(Self)
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}
