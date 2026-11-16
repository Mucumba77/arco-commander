use std::collections::BTreeMap;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Mutex;
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC,
    IEnumSTATDATA, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::ReleaseStgMedium;
use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
use windows::core::{BOOL, Error, HRESULT, Ref, Result, implement};
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::UI::Shell::{
    CFSTR_LOGICALPERFORMEDDROPEFFECT, CFSTR_PERFORMEDDROPEFFECT, CFSTR_PREFERREDDROPEFFECT,
};

const FILES: u16 = 15;
const BAD_FORMAT: HRESULT = HRESULT(0x80040064_u32 as i32);
const POINTER: HRESULT = HRESULT(0x80004003_u32 as i32);
const NOT_IMPLEMENTED: HRESULT = HRESULT(0x80004001_u32 as i32);
const NO_ADVISE: HRESULT = HRESULT(0x80040003_u32 as i32);

#[implement(IDataObject)]
struct FileData {
    files: Vec<u8>,
    formats: [u16; 3],
    effects: Mutex<BTreeMap<u16, u32>>,
}

pub(super) fn new(paths: &[PathBuf]) -> io::Result<IDataObject> {
    let mut units = Vec::new();
    for path in paths {
        if !path.is_absolute() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        std::fs::symlink_metadata(path)?;
        let name: Vec<_> = path.as_os_str().encode_wide().collect();
        if name.is_empty()
            || name.contains(&0)
            || name.len() > 32767
            || units.len() + name.len() > 8 * 1024 * 1024
        {
            return Err(io::Error::other(
                "Die Dateiliste kann nicht an das Drag-Ziel übergeben werden.",
            ));
        }
        units.extend(name);
        units.push(0);
    }
    if paths.is_empty() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    units.push(0);
    let mut files = Vec::new();
    // DROPFILES: byte offset, POINT x/y, nonclient flag, Unicode flag.
    for value in [20_u32, 0, 0, 0, 1] {
        files.extend(value.to_le_bytes());
    }
    files.extend(units.into_iter().flat_map(u16::to_le_bytes));
    let formats = [
        CFSTR_PREFERREDDROPEFFECT,
        CFSTR_PERFORMEDDROPEFFECT,
        CFSTR_LOGICALPERFORMEDDROPEFFECT,
    ]
    .map(|name| super::effect_format(name).map(|format| format.cfFormat));
    let [preferred, performed, logical] = formats;
    let formats = [preferred?, performed?, logical?];
    Ok(FileData {
        files,
        formats,
        effects: Mutex::new(BTreeMap::new()),
    }
    .into())
}

fn format(kind: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: kind,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    }
}

fn checked(pointer: *const FORMATETC) -> Result<FORMATETC> {
    if pointer.is_null() {
        return Err(Error::from_hresult(POINTER));
    }
    let value = unsafe { *pointer };
    if value.dwAspect != DVASPECT_CONTENT.0
        || value.lindex != -1
        || value.tymed & TYMED_HGLOBAL.0 as u32 == 0
    {
        return Err(Error::from_hresult(BAD_FORMAT));
    }
    Ok(value)
}

fn medium(bytes: &[u8]) -> Result<STGMEDIUM> {
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
    if memory.is_null() {
        return Err(Error::from_hresult(HRESULT(0x8007000E_u32 as i32)));
    }
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        let error = Error::from_thread();
        unsafe { GlobalFree(memory) };
        return Err(error);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
        GlobalUnlock(memory);
    }
    let mut result = STGMEDIUM {
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    };
    result.u.hGlobal = HGLOBAL(memory);
    Ok(result)
}

impl IDataObject_Impl for FileData_Impl {
    fn GetData(&self, pointer: *const FORMATETC) -> Result<STGMEDIUM> {
        let format = checked(pointer)?;
        if format.cfFormat == FILES {
            return medium(&self.files);
        }
        let values = self
            .effects
            .lock()
            .map_err(|_| Error::from_hresult(NOT_IMPLEMENTED))?;
        let value = values
            .get(&format.cfFormat)
            .ok_or_else(|| Error::from_hresult(BAD_FORMAT))?;
        medium(&value.to_le_bytes())
    }
    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(Error::from_hresult(NOT_IMPLEMENTED))
    }
    fn QueryGetData(&self, pointer: *const FORMATETC) -> HRESULT {
        match checked(pointer) {
            Ok(format)
                if format.cfFormat == FILES
                    || self
                        .effects
                        .lock()
                        .is_ok_and(|v| v.contains_key(&format.cfFormat)) =>
            {
                HRESULT(0)
            }
            Ok(_) => BAD_FORMAT,
            Err(error) => error.code(),
        }
    }
    fn GetCanonicalFormatEtc(&self, input: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        if output.is_null() {
            return POINTER;
        }
        match checked(input) {
            Ok(mut value) => {
                value.ptd = std::ptr::null_mut();
                unsafe { output.write(value) };
                HRESULT(0x00040130)
            }
            Err(error) => error.code(),
        }
    }
    fn SetData(
        &self,
        pointer: *const FORMATETC,
        storage: *const STGMEDIUM,
        release: BOOL,
    ) -> Result<()> {
        let format = checked(pointer)?;
        if storage.is_null() {
            return Err(Error::from_hresult(POINTER));
        }
        if !self.formats.contains(&format.cfFormat) {
            return Err(Error::from_hresult(BAD_FORMAT));
        }
        let storage_ref = unsafe { &*storage };
        if storage_ref.tymed != TYMED_HGLOBAL.0 as u32 {
            return Err(Error::from_hresult(BAD_FORMAT));
        }
        let handle = unsafe { storage_ref.u.hGlobal }.0;
        if handle.is_null() || unsafe { GlobalSize(handle) } < 4 {
            return Err(Error::from_hresult(BAD_FORMAT));
        }
        let pointer = unsafe { GlobalLock(handle) }.cast::<u32>();
        if pointer.is_null() {
            return Err(Error::from_thread());
        }
        let effect = unsafe { pointer.read_unaligned() };
        unsafe { GlobalUnlock(handle) };
        if effect & !7 != 0 {
            return Err(Error::from_hresult(BAD_FORMAT));
        }
        self.effects
            .lock()
            .map_err(|_| Error::from_hresult(NOT_IMPLEMENTED))?
            .insert(format.cfFormat, effect);
        if release.as_bool() {
            let mut owned = unsafe { std::ptr::read(storage) };
            unsafe { ReleaseStgMedium(&mut owned) };
        }
        Ok(())
    }
    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        let values = match direction {
            1 => std::iter::once(FILES)
                .chain(
                    self.effects
                        .lock()
                        .map_err(|_| Error::from_hresult(NOT_IMPLEMENTED))?
                        .keys()
                        .copied(),
                )
                .map(format)
                .collect::<Vec<_>>(),
            2 => self.formats.iter().copied().map(format).collect(),
            _ => return Err(Error::from_hresult(NOT_IMPLEMENTED)),
        };
        unsafe { SHCreateStdEnumFmtEtc(&values) }
    }
    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
        Err(Error::from_hresult(NO_ADVISE))
    }
    fn DUnadvise(&self, _: u32) -> Result<()> {
        Err(Error::from_hresult(NO_ADVISE))
    }
    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(Error::from_hresult(NO_ADVISE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drag::{EffectMedium, get_effect, set_effect};
    use crate::shell_items::Ole;

    #[test]
    fn fallback_file_data_preserves_unicode_paths_and_synchronous_move_reports() {
        let _ole = Ole::new().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("Grüße 🦀.txt");
        std::fs::write(&path, b"source").unwrap();
        let object = new(std::slice::from_ref(&path)).unwrap();
        assert_eq!(get_effect(&object, CFSTR_LOGICALPERFORMEDDROPEFFECT), None);
        for (name, value) in [
            (CFSTR_PREFERREDDROPEFFECT, 1),
            (CFSTR_PERFORMEDDROPEFFECT, 2),
            (CFSTR_LOGICALPERFORMEDDROPEFFECT, 2),
        ] {
            set_effect(&object, name, value).unwrap();
            assert_eq!(get_effect(&object, name), Some(value));
        }
        assert!(set_effect(&object, CFSTR_PREFERREDDROPEFFECT, u32::MAX).is_err());
        assert_eq!(get_effect(&object, CFSTR_PREFERREDDROPEFFECT), Some(1));
        let medium = EffectMedium(unsafe { object.GetData(&format(FILES)) }.unwrap());
        let handle = unsafe { medium.0.u.hGlobal }.0;
        let count = unsafe {
            windows_sys::Win32::UI::Shell::DragQueryFileW(handle, u32::MAX, std::ptr::null_mut(), 0)
        };
        assert_eq!(count, 1);
        let mut buffer = vec![0_u16; 32768];
        let length = unsafe {
            windows_sys::Win32::UI::Shell::DragQueryFileW(
                handle,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        } as usize;
        assert_eq!(
            &buffer[..length],
            path.as_os_str().encode_wide().collect::<Vec<_>>()
        );
        assert!(unsafe { object.EnumFormatEtc(1) }.is_ok());
        assert!(unsafe { object.EnumFormatEtc(2) }.is_ok());
    }

    #[test]
    fn fallback_file_data_rejects_invalid_and_relative_sources() {
        assert!(new(&[]).is_err());
        assert!(new(&[PathBuf::from("relative.txt")]).is_err());
    }
}
