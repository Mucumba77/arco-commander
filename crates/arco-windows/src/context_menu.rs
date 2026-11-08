use crate::shell_items::{Ole, item_array};
use std::io;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    BHID_SFUIObject, CMF_EXPLORE, CMF_EXTENDEDVERBS, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO,
    CMINVOKECOMMANDINFOEX, IContextMenu, IContextMenu2, IContextMenu3, SEE_MASK_NOASYNC,
    SEE_MASK_UNICODE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetMenuItemCount, HMENU, SW_SHOWNORMAL, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
};
use windows::core::{Interface, PCSTR, PCWSTR};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

const FIRST_COMMAND: u32 = 1;
const LAST_COMMAND: u32 = 0x7fff;
const SUBCLASS_ID: usize = 1;

/// Shows the Shell menu for a selection from one folder. Call on the window's UI thread.
pub fn show_context_menu(paths: &[PathBuf], point: (i32, i32), extended: bool) -> io::Result<bool> {
    let Some(first) = paths.first() else {
        return Ok(false);
    };
    let parent = first.parent();
    if paths.iter().any(|path| path.parent() != parent) {
        return Err(io::Error::other(
            "Das Windows-Kontextmenü benötigt Einträge aus demselben Ordner.",
        ));
    }
    let owner = HWND(unsafe { GetActiveWindow() });
    if owner.0.is_null() {
        return Err(io::Error::other(
            "Kein aktives Fenster für das Kontextmenü.",
        ));
    }

    let _ole = Ole::new()?;
    let items = item_array(paths)?;
    let context: IContextMenu = unsafe { items.BindToHandler(None::<&IBindCtx>, &BHID_SFUIObject) }
        .map_err(io::Error::other)?;
    let menu = PopupMenu(unsafe { CreatePopupMenu() }.map_err(io::Error::other)?);
    let flags = CMF_EXPLORE | if extended { CMF_EXTENDEDVERBS } else { 0 };
    unsafe { context.QueryContextMenu(menu.0, 0, FIRST_COMMAND, LAST_COMMAND, flags) }
        .ok()
        .map_err(io::Error::other)?;
    if unsafe { GetMenuItemCount(Some(menu.0)) } <= 0 {
        return Err(io::Error::other("Windows liefert keine Menüeinträge."));
    }
    let messages = MenuMessages::attach(owner, &context)?;
    let command = unsafe {
        TrackPopupMenuEx(
            menu.0,
            (TPM_RIGHTBUTTON | TPM_RETURNCMD).0,
            point.0,
            point.1,
            owner,
            None,
        )
    }
    .0 as u32;
    drop(messages);
    if command == 0 {
        return Ok(false);
    }
    if !(FIRST_COMMAND..=LAST_COMMAND).contains(&command) {
        return Err(io::Error::other("Ungültiger Windows-Menübefehl."));
    }
    invoke(
        &context,
        owner,
        parent.filter(|path| path.is_absolute()),
        point,
        command - FIRST_COMMAND,
    )?;
    Ok(true)
}

fn invoke(
    context: &IContextMenu,
    owner: HWND,
    parent: Option<&Path>,
    point: (i32, i32),
    offset: u32,
) -> io::Result<()> {
    let directory = parent.map(crate::platform::wide).transpose()?;
    let request = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: SEE_MASK_UNICODE | SEE_MASK_NOASYNC | CMIC_MASK_PTINVOKE,
        hwnd: owner,
        // Windows uses pointer-sized integer offsets for menu commands.
        lpVerb: PCSTR(offset as usize as *const u8),
        lpVerbW: PCWSTR(offset as usize as *const u16),
        lpDirectoryW: directory
            .as_ref()
            .map_or(PCWSTR::null(), |path| PCWSTR(path.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: POINT {
            x: point.0,
            y: point.1,
        },
        ..Default::default()
    };
    unsafe { context.InvokeCommand((&raw const request).cast::<CMINVOKECOMMANDINFO>()) }
        .map_err(io::Error::other)
}

struct PopupMenu(HMENU);

impl Drop for PopupMenu {
    fn drop(&mut self) {
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

enum MenuHandler {
    V3(IContextMenu3),
    V2(IContextMenu2),
}

struct MenuMessages {
    owner: HWND,
    _handler: Box<MenuHandler>,
}

impl MenuMessages {
    fn attach(owner: HWND, menu: &IContextMenu) -> io::Result<Option<Self>> {
        let handler = if let Ok(menu) = menu.cast::<IContextMenu3>() {
            MenuHandler::V3(menu)
        } else if let Ok(menu) = menu.cast::<IContextMenu2>() {
            MenuHandler::V2(menu)
        } else {
            return Ok(None);
        };
        let handler = Box::new(handler);
        let reference = (&*handler as *const MenuHandler) as usize;
        // The boxed handler stays alive on this thread until the subclass is removed.
        if unsafe { SetWindowSubclass(owner.0, Some(menu_message), SUBCLASS_ID, reference) } == 0 {
            return Err(io::Error::other(
                "Windows-Menünachrichten konnten nicht verbunden werden.",
            ));
        }
        Ok(Some(Self {
            owner,
            _handler: handler,
        }))
    }
}

impl Drop for MenuMessages {
    fn drop(&mut self) {
        unsafe { RemoveWindowSubclass(self.owner.0, Some(menu_message), SUBCLASS_ID) };
    }
}

unsafe extern "system" fn menu_message(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    _: usize,
    reference: usize,
) -> isize {
    if matches!(
        message,
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
    ) {
        // The subclass's registration owns the reference for the duration of the popup.
        let handler = unsafe { &*(reference as *const MenuHandler) };
        let mut result = LRESULT(0);
        let handled = match handler {
            MenuHandler::V3(menu) => unsafe {
                menu.HandleMenuMsg2(message, WPARAM(wparam), LPARAM(lparam), Some(&mut result))
            }
            .is_ok(),
            MenuHandler::V2(menu) if message != WM_MENUCHAR => {
                unsafe { menu.HandleMenuMsg(message, WPARAM(wparam), LPARAM(lparam)) }.is_ok()
            }
            MenuHandler::V2(_) => false,
        };
        if handled {
            return result.0;
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}
