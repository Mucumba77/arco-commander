use crate::FileDragOutcome;
use crate::shell_items::{Ole, item_array};
use std::io;
use std::path::PathBuf;
use std::ptr::null_mut;
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IBindCtx, IDataObject, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::ReleaseStgMedium;
use windows::Win32::UI::Shell::{BHID_DataObject, IDataObjectAsyncCapability};
use windows::core::{HRESULT, Interface};
use windows_sys::Win32::Foundation::{DRAGDROP_S_DROP, E_NOINTERFACE};
use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{
    DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetActiveWindow, GetAsyncKeyState, VK_CONTROL, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::{
    CFSTR_LOGICALPERFORMEDDROPEFFECT, CFSTR_PERFORMEDDROPEFFECT, CFSTR_PREFERREDDROPEFFECT,
    SHDoDragDrop,
};

mod source;

pub fn file_drop_action() -> crate::FileDropAction {
    let pressed = |key| unsafe { GetAsyncKeyState(i32::from(key)) < 0 };
    crate::FileDropAction::from_modifiers(pressed(VK_CONTROL), pressed(VK_SHIFT))
}

pub fn drag_files(paths: &[PathBuf]) -> io::Result<FileDragOutcome> {
    if paths.is_empty() {
        return Ok(FileDragOutcome::NoConfirmedMove);
    }
    let _ole = Ole::new()?;
    let (data, allowed) = prepare_drag_data(paths)?;
    let asynchronous = data.cast::<IDataObjectAsyncCapability>();
    let async_disabled = allowed & DROPEFFECT_MOVE != 0
        && match &asynchronous {
            // Read the target's final report when the drag returns.
            Ok(capability) => unsafe { capability.SetAsyncMode(false) }.is_ok(),
            Err(error) => error.code().0 == E_NOINTERFACE,
        };
    let sources = async_disabled
        .then(|| source::Sources::capture(paths).ok())
        .flatten();

    // Use the raw HRESULT: the safe wrapper also treats a canceled drop as Ok.
    let mut effect = DROPEFFECT_NONE;
    let result = unsafe {
        SHDoDragDrop(
            GetActiveWindow(),
            data.as_raw(),
            null_mut(),
            allowed,
            &mut effect,
        )
    };
    HRESULT(result).ok().map_err(io::Error::other)?;
    if result != DRAGDROP_S_DROP || allowed & DROPEFFECT_MOVE == 0 {
        return Ok(FileDragOutcome::NoConfirmedMove);
    }
    let in_operation = asynchronous
        .as_ref()
        .ok()
        .and_then(|capability| unsafe { capability.InOperation() }.ok())
        .map(|in_operation| in_operation.as_bool());
    let performed = get_effect(&data, CFSTR_PERFORMEDDROPEFFECT);
    let logical = get_effect(&data, CFSTR_LOGICALPERFORMEDDROPEFFECT);
    let synchronous = async_disabled && (asynchronous.is_err() || in_operation == Some(false));
    if classic_move_confirmed(effect, performed, logical, synchronous) {
        let completed = sources.map_or(0, source::Sources::complete);
        return Ok(if completed == paths.len() {
            FileDragOutcome::MovedAfterCopy
        } else {
            FileDragOutcome::MovePartiallyCompleted {
                completed,
                total: paths.len(),
            }
        });
    }
    Ok(move_outcome(
        effect,
        performed,
        logical,
        in_operation == Some(true),
    ))
}

fn classic_move_confirmed(
    effect: u32,
    performed: Option<u32>,
    logical: Option<u32>,
    synchronous: bool,
) -> bool {
    synchronous
        && effect == DROPEFFECT_MOVE
        && performed == Some(DROPEFFECT_MOVE)
        && logical == Some(DROPEFFECT_MOVE)
}

fn prepare_drag_data(paths: &[PathBuf]) -> io::Result<(IDataObject, u32)> {
    let items = item_array(paths)?;
    let data: IDataObject = unsafe { items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) }
        .map_err(io::Error::other)?;
    // Preserve copy as the default. If the data object cannot express that
    // preference, keep the previous copy/link-only effects.
    let mut allowed = DROPEFFECT_COPY | DROPEFFECT_LINK;
    if set_effect(&data, CFSTR_PREFERREDDROPEFFECT, DROPEFFECT_COPY).is_ok() {
        allowed |= DROPEFFECT_MOVE;
    }
    Ok((data, allowed))
}

fn move_outcome(
    effect: u32,
    performed: Option<u32>,
    logical: Option<u32>,
    pending: bool,
) -> FileDragOutcome {
    // Optimized Shell moves are entirely performed by the target. Ambiguous
    // reports stay informational and do not reach the source cleanup path.
    if !pending
        && matches!(effect, DROPEFFECT_NONE | DROPEFFECT_COPY)
        && performed == Some(DROPEFFECT_NONE)
        && logical == Some(DROPEFFECT_MOVE)
    {
        FileDragOutcome::MovedByTarget
    } else if effect & DROPEFFECT_MOVE != 0
        || performed.is_some_and(|effect| effect & DROPEFFECT_MOVE != 0)
        || logical.is_some_and(|effect| effect & DROPEFFECT_MOVE != 0)
    {
        FileDragOutcome::MoveUnconfirmed
    } else {
        FileDragOutcome::NoConfirmedMove
    }
}

fn effect_format(name: windows_sys::core::PCWSTR) -> io::Result<FORMATETC> {
    let format = unsafe { RegisterClipboardFormatW(name) };
    if format == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FORMATETC {
        cfFormat: format as u16,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    })
}

struct EffectMedium(STGMEDIUM);

impl Drop for EffectMedium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

fn set_effect(data: &IDataObject, name: windows_sys::core::PCWSTR, effect: u32) -> io::Result<()> {
    let format = effect_format(name)?;
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>()) };
    if memory.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut medium = EffectMedium(STGMEDIUM::default());
    medium.0.tymed = format.tymed;
    medium.0.u.hGlobal = HGLOBAL(memory);
    let pointer = unsafe { GlobalLock(memory) }.cast::<u32>();
    if pointer.is_null() {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        pointer.write_unaligned(effect);
        GlobalUnlock(memory);
        data.SetData(&format, &medium.0, true)
    }
    .map_err(io::Error::other)?;
    // A successful SetData call transfers ownership of the medium.
    std::mem::forget(medium);
    Ok(())
}

fn get_effect(data: &IDataObject, name: windows_sys::core::PCWSTR) -> Option<u32> {
    let format = effect_format(name).ok()?;
    let medium = EffectMedium(unsafe { data.GetData(&format) }.ok()?);
    if medium.0.tymed != format.tymed {
        return None;
    }
    let memory = unsafe { medium.0.u.hGlobal }.0;
    if memory.is_null() || unsafe { GlobalSize(memory) } < size_of::<u32>() {
        return None;
    }
    let pointer = unsafe { GlobalLock(memory) }.cast::<u32>();
    if pointer.is_null() {
        return None;
    }
    let effect = unsafe { pointer.read_unaligned() };
    unsafe { GlobalUnlock(memory) };
    Some(effect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_completed_optimized_move_confirms_source_changes() {
        use FileDragOutcome::*;
        assert_eq!(
            move_outcome(
                DROPEFFECT_COPY,
                Some(DROPEFFECT_NONE),
                Some(DROPEFFECT_MOVE),
                false
            ),
            MovedByTarget
        );
        assert_eq!(
            move_outcome(
                DROPEFFECT_NONE,
                Some(DROPEFFECT_NONE),
                Some(DROPEFFECT_MOVE),
                true
            ),
            MoveUnconfirmed
        );
        assert_eq!(
            move_outcome(
                DROPEFFECT_MOVE,
                Some(DROPEFFECT_MOVE),
                Some(DROPEFFECT_MOVE),
                false
            ),
            MoveUnconfirmed
        );
        assert_eq!(
            move_outcome(DROPEFFECT_MOVE, None, None, false),
            MoveUnconfirmed
        );
        assert_eq!(
            move_outcome(
                DROPEFFECT_LINK,
                Some(DROPEFFECT_NONE),
                Some(DROPEFFECT_MOVE),
                false
            ),
            MoveUnconfirmed
        );
        assert_eq!(
            move_outcome(
                DROPEFFECT_COPY,
                Some(DROPEFFECT_COPY),
                Some(DROPEFFECT_COPY),
                false
            ),
            NoConfirmedMove
        );
        assert_eq!(
            move_outcome(DROPEFFECT_NONE, None, None, false),
            NoConfirmedMove
        );
    }

    #[test]
    fn classic_move_requires_all_three_effects_and_a_synchronous_drop() {
        assert!(classic_move_confirmed(
            DROPEFFECT_MOVE,
            Some(DROPEFFECT_MOVE),
            Some(DROPEFFECT_MOVE),
            true
        ));
        for (returned, performed, logical, synchronous) in [
            (
                DROPEFFECT_MOVE,
                Some(DROPEFFECT_MOVE),
                Some(DROPEFFECT_MOVE),
                false,
            ),
            (DROPEFFECT_MOVE, Some(DROPEFFECT_MOVE), None, true),
            (DROPEFFECT_MOVE, None, Some(DROPEFFECT_MOVE), true),
            (
                DROPEFFECT_MOVE,
                Some(DROPEFFECT_NONE),
                Some(DROPEFFECT_MOVE),
                true,
            ),
            (
                DROPEFFECT_COPY,
                Some(DROPEFFECT_MOVE),
                Some(DROPEFFECT_MOVE),
                true,
            ),
        ] {
            assert!(!classic_move_confirmed(
                returned,
                performed,
                logical,
                synchronous
            ));
        }
    }

    #[test]
    fn native_shell_drag_data_retains_the_preferred_and_reported_effects() {
        let _ole = Ole::new().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let (data, allowed) = prepare_drag_data(&[file.path().to_path_buf()]).unwrap();
        assert_eq!(allowed, DROPEFFECT_COPY | DROPEFFECT_LINK | DROPEFFECT_MOVE);
        assert_eq!(
            get_effect(&data, CFSTR_PREFERREDDROPEFFECT),
            Some(DROPEFFECT_COPY)
        );
        let files = FORMATETC {
            cfFormat: windows_sys::Win32::System::Ole::CF_HDROP,
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        let _files = EffectMedium(unsafe { data.GetData(&files) }.unwrap());
        for (format, effect) in [
            (CFSTR_PERFORMEDDROPEFFECT, DROPEFFECT_NONE),
            (CFSTR_LOGICALPERFORMEDDROPEFFECT, DROPEFFECT_MOVE),
        ] {
            set_effect(&data, format, effect).unwrap();
            assert_eq!(get_effect(&data, format), Some(effect));
        }
    }
}
