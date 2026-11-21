use crate::platform::wide;
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use windows::Win32::Foundation::{E_ACCESSDENIED, PROPERTYKEY, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PVCHF_DEFAULT, PropVariantChangeType,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, ShellLink,
};
use windows::core::{GUID, Interface, PCWSTR};

// System.Title from propkey.h.
const TITLE: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
    pid: 2,
};
const MAX_ARGUMENTS: usize = 4096;

/// Updates the current application's taskbar category. Returns arguments removed by the user.
pub fn publish(
    exe: &Path,
    category: &str,
    entries: &[(String, String)],
) -> io::Result<BTreeSet<String>> {
    let source = wide(exe)?;
    let category = wide(Path::new(category))?;
    let _apartment = Apartment::enter()?;
    // The shell infers the application ID from Arco's running process.
    let list: ICustomDestinationList =
        unsafe { CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER) }
            .map_err(io::Error::other)?;
    let mut slots = 0;
    let removed: IObjectArray = unsafe { list.BeginList(&mut slots) }.map_err(io::Error::other)?;
    let result = (|| {
        let excluded = removed_arguments(&removed)?;
        let collection: IObjectCollection =
            unsafe { CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)? };
        let mut used = BTreeSet::new();
        for (title, args) in entries
            .iter()
            .filter(|(_, args)| !excluded.contains(args) && used.insert(args))
            .take(slots as usize)
        {
            let link = destination(PCWSTR(source.as_ptr()), title, args)?;
            unsafe { collection.AddObject(&link)? };
        }
        if unsafe { collection.GetCount()? } != 0 {
            let objects: IObjectArray = collection.cast()?;
            unsafe { list.AppendCategory(PCWSTR(category.as_ptr()), &objects)? };
        }
        unsafe { list.CommitList()? };
        Ok::<_, windows::core::Error>(excluded)
    })();
    if result.is_err() {
        let _ = unsafe { list.AbortList() };
    }
    result.map_err(|error| {
        if error.code() == E_ACCESSDENIED {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Windows hat die Sprungliste gesperrt.",
            )
        } else {
            io::Error::other(error)
        }
    })
}

fn destination(source: PCWSTR, title: &str, args: &str) -> windows::core::Result<IShellLinkW> {
    use windows::Win32::Foundation::E_INVALIDARG;
    if title.is_empty()
        || args.is_empty()
        || title.chars().any(char::is_control)
        || args.chars().any(char::is_control)
        || args.encode_utf16().count() > MAX_ARGUMENTS
    {
        return Err(E_INVALIDARG.into());
    }
    let args = args.encode_utf16().chain([0]).collect::<Vec<_>>();
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
    let properties: IPropertyStore = link.cast()?;
    let mut title_value = PROPVARIANT::default();
    let title = PROPVARIANT::from(title);
    // Shell destinations require a VT_LPWSTR System.Title.
    unsafe {
        link.SetPath(source)?;
        link.SetIconLocation(source, 0)?;
        link.SetArguments(PCWSTR(args.as_ptr()))?;
        PropVariantChangeType(&mut title_value, &title, PVCHF_DEFAULT, VT_LPWSTR)?;
        properties.SetValue(&TITLE, &title_value)?;
        properties.Commit()?;
    }
    Ok(link)
}

fn removed_arguments(removed: &IObjectArray) -> windows::core::Result<BTreeSet<String>> {
    let mut arguments = BTreeSet::new();
    let mut buffer = vec![0; MAX_ARGUMENTS + 1];
    for index in 0..unsafe { removed.GetCount()? } {
        if let Ok(link) = unsafe { removed.GetAt::<IShellLinkW>(index) } {
            unsafe { link.GetArguments(&mut buffer)? };
            let end = buffer
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(buffer.len());
            arguments.insert(String::from_utf16_lossy(&buffer[..end]));
        }
    }
    Ok(arguments)
}

struct Apartment(bool);

impl Apartment {
    fn enter() -> io::Result<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if status != RPC_E_CHANGED_MODE {
            status.ok().map_err(io::Error::other)?;
        }
        Ok(Self(status.is_ok()))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToBSTR;

    #[test]
    fn taskbar_link_exposes_title_and_arguments_without_publishing() {
        let _apartment = Apartment::enter().unwrap();
        let source = wide(std::env::current_exe().unwrap()).unwrap();
        let args = r#"--profile "C:\example folder\profile.json" --workspace "Atlas Übersicht""#;
        let link = destination(PCWSTR(source.as_ptr()), "Atlas Übersicht", args).unwrap();
        let store: IPropertyStore = link.cast().unwrap();
        let value = unsafe { store.GetValue(&TITLE) }.unwrap();
        assert_eq!(
            unsafe { PropVariantToBSTR(&value) }.unwrap().to_string(),
            "Atlas Übersicht"
        );
        let mut buffer = [0; 256];
        unsafe { link.GetArguments(&mut buffer) }.unwrap();
        let end = buffer.iter().position(|unit| *unit == 0).unwrap();
        assert_eq!(String::from_utf16(&buffer[..end]).unwrap(), args);
    }
}
