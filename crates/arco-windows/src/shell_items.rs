use crate::{
    SHELL_APPLICATIONS, SHELL_COMPUTER, SHELL_DESKTOP, SHELL_LIBRARIES, SHELL_NETWORK,
    SHELL_RECYCLE_BIN, ShellFolder, ShellListing,
};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::SystemServices::SFGAO_FOLDER;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    FOLDERID_AppsFolder, FOLDERID_Desktop, FOLDERID_Libraries, FOLDERID_NetworkFolder,
    FOLDERID_RecycleBinFolder, IEnumIDList, ILCombine, ILRemoveLastID, IShellFolder,
    IShellItemArray, SHCONTF_FOLDERS, SHCONTF_INCLUDEHIDDEN, SHCONTF_NONFOLDERS,
    SHCreateShellItemArrayFromIDLists, SHGetDesktopFolder, SHGetKnownFolderIDList,
    SHGetNameFromIDList, SHParseDisplayName, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
};
use windows::core::{GUID, PCWSTR};

mod operations;
pub use operations::shell_transfer;

pub(crate) fn item_array(paths: &[PathBuf]) -> io::Result<IShellItemArray> {
    let ids = paths
        .iter()
        .map(|path| parse_id(path))
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

fn parse_id(value: impl AsRef<std::ffi::OsStr>) -> io::Result<Pidl> {
    let value = value.as_ref();
    if let Some((_, folder)) = known_virtual_folder(value) {
        let id =
            Pidl(unsafe { SHGetKnownFolderIDList(&folder, 0, None) }.map_err(io::Error::other)?);
        return if id.0.is_null() {
            Err(io::Error::other("Windows hat keine Shell-ID geliefert."))
        } else {
            Ok(id)
        };
    }
    let name = crate::platform::wide(value)?;
    let mut id = Pidl(null_mut());
    unsafe { SHParseDisplayName(PCWSTR(name.as_ptr()), None::<&IBindCtx>, &mut id.0, 0, None) }
        .map_err(io::Error::other)?;
    if id.0.is_null() {
        return Err(io::Error::other("Windows hat keine Shell-ID geliefert."));
    }
    Ok(id)
}

fn known_virtual_folder(value: &std::ffi::OsStr) -> Option<(&'static str, GUID)> {
    [
        (SHELL_NETWORK, FOLDERID_NetworkFolder),
        (SHELL_LIBRARIES, FOLDERID_Libraries),
        (SHELL_APPLICATIONS, FOLDERID_AppsFolder),
        (SHELL_RECYCLE_BIN, FOLDERID_RecycleBinFolder),
    ]
    .into_iter()
    .find(|(address, _)| value.eq_ignore_ascii_case(std::ffi::OsStr::new(address)))
}

fn name(id: &Pidl, form: SIGDN) -> io::Result<OsString> {
    let name = unsafe { SHGetNameFromIDList(id.0, form) }.map_err(io::Error::other)?;
    if name.0.is_null() {
        return Err(io::Error::other(
            "Windows hat keinen Shell-Namen geliefert.",
        ));
    }
    let value = unsafe { name.to_hstring() }.to_os_string();
    unsafe { CoTaskMemFree(Some(name.0.cast())) };
    Ok(value)
}

pub fn shell_folders(parent: Option<&str>, hidden: bool) -> io::Result<Vec<ShellFolder>> {
    Ok(list_namespace(
        parent.unwrap_or(SHELL_DESKTOP),
        hidden,
        false,
        &AtomicBool::new(false),
    )?
    .entries)
}

pub fn shell_listing(parent: &str, hidden: bool, cancel: &AtomicBool) -> io::Result<ShellListing> {
    list_namespace(parent, hidden, true, cancel)
}

fn list_namespace(
    parent: &str,
    hidden: bool,
    include_files: bool,
    cancel: &AtomicBool,
) -> io::Result<ShellListing> {
    let _apartment = Ole::new()?;
    let desktop = unsafe { SHGetDesktopFolder() }.map_err(io::Error::other)?;
    let root = parent.eq_ignore_ascii_case(SHELL_DESKTOP);
    let id = if root {
        Pidl(
            unsafe { SHGetKnownFolderIDList(&FOLDERID_Desktop, 0, None) }
                .map_err(io::Error::other)?,
        )
    } else {
        parse_id(parent)?
    };
    let root = root || id_is_empty(&id);
    let folder: IShellFolder = if root {
        desktop
    } else {
        unsafe { desktop.BindToObject(id.0, None::<&IBindCtx>) }.map_err(io::Error::other)?
    };
    let mut flags = SHCONTF_FOLDERS.0;
    if include_files {
        flags |= SHCONTF_NONFOLDERS.0;
    }
    if hidden {
        flags |= SHCONTF_INCLUDEHIDDEN.0;
    }
    let mut children: Option<IEnumIDList> = None;
    unsafe { folder.EnumObjects(Default::default(), flags as u32, &mut children) }
        .ok()
        .map_err(io::Error::other)?;
    let mut nodes = Vec::new();
    if let Some(children) = children {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let mut slot = [null_mut()];
            let status = unsafe { children.Next(&mut slot, None) };
            let child = Pidl(slot[0]);
            status.ok().map_err(io::Error::other)?;
            if child.0.is_null() {
                break;
            }
            let absolute = Pidl(unsafe { ILCombine(Some(id.0), Some(child.0)) });
            if absolute.0.is_null() {
                return Err(io::Error::other(
                    "Windows konnte die Shell-ID nicht auflösen.",
                ));
            }
            let Ok((parsing_name, title)) = describe(&absolute) else {
                continue;
            };
            let mut attributes = SFGAO_FOLDER.0;
            let directory = !include_files
                || (unsafe { folder.GetAttributesOf(&[child.0.cast_const()], &mut attributes) }
                    .is_ok()
                    && attributes & SFGAO_FOLDER.0 != 0);
            let path = name(&absolute, SIGDN_FILESYSPATH).ok().map(PathBuf::from);
            nodes.push(ShellFolder {
                name: title,
                computer: parsing_name.eq_ignore_ascii_case(SHELL_COMPUTER),
                parsing_name,
                path,
                directory,
            });
        }
    }
    nodes.sort_by_cached_key(|entry| entry.name.to_lowercase());
    let mut ancestors = Vec::new();
    if include_files && !root {
        while !id_is_empty(&id) {
            ancestors.push(describe(&id)?);
            if !unsafe { ILRemoveLastID(Some(id.0)) }.as_bool() {
                break;
            }
        }
        ancestors.reverse();
        if let Some((address, _)) = known_virtual_folder(parent.as_ref())
            && let Some((current, _)) = ancestors.last_mut()
        {
            // Some providers, including AppsFolder, return a display parsing
            // name that SHParseDisplayName cannot resolve on its own.
            *current = address.into();
        }
    }
    Ok(ShellListing {
        ancestors,
        entries: nodes,
    })
}

fn id_is_empty(id: &Pidl) -> bool {
    id.0.is_null() || unsafe { (*id.0).mkid.cb == 0 }
}

fn describe(id: &Pidl) -> io::Result<(String, String)> {
    let address = name(id, SIGDN_DESKTOPABSOLUTEPARSING)?
        .into_string()
        .map_err(|_| io::Error::other("Shell-Pfad enthält ungültigen Unicode."))?;
    let title = name(id, SIGDN_NORMALDISPLAY).map_or_else(
        |_| address.clone(),
        |title| title.to_string_lossy().into_owned(),
    );
    Ok((address, title))
}

pub fn open_shell_item(parsing_name: &str) -> io::Result<()> {
    use windows_sys::Win32::UI::Shell::{SEE_MASK_IDLIST, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let _apartment = Ole::new()?;
    let id = parse_id(parsing_name)?;
    let mut request = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_IDLIST,
        lpIDList: id.0.cast(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut request) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_virtual_locations_resolve_to_shell_addresses() {
        let _apartment = Ole::new().unwrap();
        for alias in [
            SHELL_NETWORK,
            SHELL_LIBRARIES,
            SHELL_APPLICATIONS,
            SHELL_RECYCLE_BIN,
        ] {
            let id = parse_id(alias).unwrap_or_else(|error| panic!("{alias}: {error}"));
            assert!(!id_is_empty(&id), "{alias}");
            let (_, title) = describe(&id).unwrap();
            assert!(!title.is_empty(), "{alias}");
        }
        let apps = shell_listing(SHELL_APPLICATIONS, false, &AtomicBool::new(false)).unwrap();
        let (address, _) = apps.ancestors.last().unwrap();
        assert_eq!(address, SHELL_APPLICATIONS);
        assert!(!id_is_empty(&parse_id(address).unwrap()));
    }

    #[test]
    fn desktop_namespace_contains_a_browsable_computer_branch() {
        let desktop = super::shell_folders(None, false).unwrap();
        let computer = desktop.iter().find(|entry| entry.computer).unwrap();
        let volumes = super::shell_folders(Some(&computer.parsing_name), false).unwrap();
        assert!(
            volumes
                .iter()
                .any(|entry| entry.path.as_ref().is_some_and(|path| path.is_absolute()))
        );
    }
}
