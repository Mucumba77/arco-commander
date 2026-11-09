use super::{Ole, parse_id};
use crate::{ShellAction, ShellTransferOptions, ShellTransferReport};
use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows::Win32::Foundation::{E_ABORT, ERROR_CANCELLED, HWND, S_OK};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::SystemServices::{
    SFGAO_CANCOPY, SFGAO_CANDELETE, SFGAO_CANMOVE, SFGAO_CANRENAME, SFGAO_FLAGS, SFGAO_FOLDER,
};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_USER_CANCELLED, COPYENGINE_S_ALREADY_DONE, COPYENGINE_S_COLLISIONRESOLVED,
    COPYENGINE_S_DONT_PROCESS_CHILDREN, COPYENGINE_S_KEEP_BOTH, COPYENGINE_S_MERGE, FOF_ALLOWUNDO,
    FOF_NO_UI, FOF_NOERRORUI, FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD, FOFX_EARLYFAILURE,
    FOFX_RECYCLEONDELETE, FOFX_SHOWELEVATIONPROMPT, FileOperation, IFileOperation,
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
    SHCreateItemFromIDList,
};
use windows::core::{HRESULT, PCWSTR, Ref, Result, implement};

fn item(path: &Path, required: SFGAO_FLAGS) -> io::Result<IShellItem> {
    let pidl = parse_id(path)?;
    let item: IShellItem = unsafe { SHCreateItemFromIDList(pidl.0) }.map_err(io::Error::other)?;
    let attributes = unsafe { item.GetAttributes(required) }.map_err(io::Error::other)?;
    if attributes.0 & required.0 != required.0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Der Windows-Shell-Eintrag unterstützt diese Aktion nicht.",
        ));
    }
    Ok(item)
}

pub fn shell_transfer(
    sources: &[PathBuf],
    action: &ShellAction,
    options: ShellTransferOptions,
    cancel: Arc<AtomicBool>,
    progress: impl FnMut(&Path),
) -> io::Result<ShellTransferReport> {
    transfer(sources, action, options, cancel, progress, false)
}

fn transfer(
    sources: &[PathBuf],
    action: &ShellAction,
    options: ShellTransferOptions,
    cancel: Arc<AtomicBool>,
    mut progress: impl FnMut(&Path),
    quiet: bool,
) -> io::Result<ShellTransferReport> {
    let _ole = Ole::new()?;
    let destination = match action {
        ShellAction::Copy(target) | ShellAction::Move(target) => Some(item(target, SFGAO_FOLDER)?),
        _ => None,
    };
    let required = match action {
        ShellAction::Copy(_) => SFGAO_CANCOPY,
        ShellAction::Move(_) => SFGAO_CANMOVE,
        ShellAction::Recycle | ShellAction::Delete => SFGAO_CANDELETE,
        ShellAction::Rename(_) => SFGAO_CANRENAME,
        ShellAction::NewFolder(_) => SFGAO_FOLDER,
    };
    let name = match action {
        ShellAction::Rename(name) | ShellAction::NewFolder(name) => crate::platform::wide(name)?,
        _ => Vec::new(),
    };
    let mut report = ShellTransferReport::default();
    for path in sources {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(path);
        let result: io::Result<bool> = (|| {
            let source = item(path, required)?;
            let operation: IFileOperation =
                unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER) }
                    .map_err(io::Error::other)?;
            let state = Rc::new(Status::default());
            let sink: IFileOperationProgressSink = Progress {
                state: state.clone(),
                cancel: cancel.clone(),
            }
            .into();
            let mut flags = FOF_NOERRORUI | FOFX_EARLYFAILURE;
            if quiet {
                flags |= FOF_NO_UI;
            }
            if options.allow_elevation {
                flags |= FOFX_SHOWELEVATIONPROMPT;
            }
            if matches!(action, ShellAction::Recycle) {
                flags |= FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_WANTNUKEWARNING;
            } else if !matches!(action, ShellAction::Delete) && !quiet {
                flags |= FOFX_ADDUNDORECORD;
            }
            unsafe { operation.SetOperationFlags(flags) }.map_err(io::Error::other)?;
            if let Some(owner) = options.owner {
                unsafe { operation.SetOwnerWindow(HWND(owner as *mut _)) }
                    .map_err(io::Error::other)?;
            }
            let cookie = unsafe { operation.Advise(&sink) }.map_err(io::Error::other)?;
            let queued = unsafe {
                match action {
                    ShellAction::Copy(_) => operation.CopyItem(
                        &source,
                        destination.as_ref().expect("copy destination"),
                        PCWSTR::null(),
                        None::<&IFileOperationProgressSink>,
                    ),
                    ShellAction::Move(_) => operation.MoveItem(
                        &source,
                        destination.as_ref().expect("move destination"),
                        PCWSTR::null(),
                        None::<&IFileOperationProgressSink>,
                    ),
                    ShellAction::Recycle | ShellAction::Delete => {
                        operation.DeleteItem(&source, None::<&IFileOperationProgressSink>)
                    }
                    ShellAction::Rename(_) => operation.RenameItem(
                        &source,
                        PCWSTR(name.as_ptr()),
                        None::<&IFileOperationProgressSink>,
                    ),
                    ShellAction::NewFolder(_) => operation.NewItem(
                        &source,
                        0x10,
                        PCWSTR(name.as_ptr()),
                        PCWSTR::null(),
                        None::<&IFileOperationProgressSink>,
                    ),
                }
            };
            let result = queued.and_then(|()| unsafe { operation.PerformOperations() });
            let aborted = unsafe { operation.GetAnyOperationsAborted() };
            let _ = unsafe { operation.Unadvise(cookie) };
            if cancel.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if let Some(error) = state
                .error
                .get()
                .or_else(|| result.err().map(|error| error.code()))
            {
                if matches!(error, E_ABORT | COPYENGINE_E_USER_CANCELLED)
                    || error == HRESULT::from_win32(ERROR_CANCELLED.0)
                {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                return Err(io::Error::other(windows::core::Error::from_hresult(error)));
            }
            if aborted.map_err(io::Error::other)?.as_bool() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            Ok(state.completed.get())
        })();
        match result {
            Ok(true) => report.completed += 1,
            Ok(false) => report.skipped += 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                report.cancelled = true;
                break;
            }
            Err(error) => report.errors.push(format!("{}: {error}", path.display())),
        }
    }
    Ok(report)
}

#[derive(Default)]
struct Status {
    completed: Cell<bool>,
    error: Cell<Option<HRESULT>>,
}

#[implement(IFileOperationProgressSink)]
struct Progress {
    state: Rc<Status>,
    cancel: Arc<AtomicBool>,
}

impl Progress {
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(E_ABORT.into())
        } else {
            Ok(())
        }
    }

    fn record(&self, result: HRESULT) {
        if result.is_err() && self.state.error.get().is_none() {
            self.state.error.set(Some(result));
        }
    }

    fn post(&self, result: HRESULT) -> Result<()> {
        self.record(result);
        if matches!(
            result,
            S_OK | COPYENGINE_S_ALREADY_DONE
                | COPYENGINE_S_COLLISIONRESOLVED
                | COPYENGINE_S_DONT_PROCESS_CHILDREN
                | COPYENGINE_S_KEEP_BOTH
                | COPYENGINE_S_MERGE
        ) {
            self.state.completed.set(true);
        }
        self.check()
    }
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for Progress_Impl {
    fn StartOperations(&self) -> Result<()> {
        self.check()
    }
    fn FinishOperations(&self, result: HRESULT) -> Result<()> {
        self.record(result);
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        self.check()
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        result: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.post(result)
    }
    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        self.check()
    }
    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        result: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.post(result)
    }
    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        self.check()
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        result: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.post(result)
    }
    fn PreDeleteItem(&self, _: u32, _: Ref<IShellItem>) -> Result<()> {
        self.check()
    }
    fn PostDeleteItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        result: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.post(result)
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        self.check()
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        result: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        self.post(result)
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> Result<()> {
        self.check()
    }
    fn ResetTimer(&self) -> Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> Result<()> {
        self.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    #[ignore = "requires the Windows compressed folders Shell provider"]
    fn shell_transfer_copies_a_virtual_zip_member_without_a_local_path() {
        const ZIP: &[u8] = &[
            80, 75, 3, 4, 20, 0, 0, 0, 0, 0, 121, 163, 105, 93, 57, 86, 154, 164, 13, 0, 0, 0, 13,
            0, 0, 0, 10, 0, 0, 0, 77, 101, 109, 98, 101, 114, 46, 116, 120, 116, 83, 104, 101, 108,
            108, 32, 102, 105, 120, 116, 117, 114, 101, 80, 75, 1, 2, 20, 0, 20, 0, 0, 0, 0, 0,
            121, 163, 105, 93, 57, 86, 154, 164, 13, 0, 0, 0, 13, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 128, 1, 0, 0, 0, 0, 77, 101, 109, 98, 101, 114, 46, 116, 120, 116, 80, 75,
            5, 6, 0, 0, 0, 0, 1, 0, 1, 0, 56, 0, 0, 0, 53, 0, 0, 0, 0, 0,
        ];
        let fixture = tempfile::tempdir().unwrap();
        let archive = fixture.path().join("Fixture.zip");
        fs::write(&archive, ZIP).unwrap();
        let target = fixture.path().join("Ziel");
        fs::create_dir(&target).unwrap();
        let listing =
            crate::shell_listing(archive.to_str().unwrap(), false, &AtomicBool::new(false))
                .unwrap();
        let entry = listing
            .entries
            .into_iter()
            .find(|entry| entry.parsing_name.ends_with("Member.txt"))
            .unwrap();
        assert!(entry.path.is_none());
        let report = transfer(
            &[PathBuf::from(entry.parsing_name)],
            &ShellAction::Copy(target.clone()),
            ShellTransferOptions::default(),
            Arc::new(AtomicBool::new(false)),
            |_| {},
            true,
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 1);
        assert_eq!(
            fs::read(target.join("Member.txt")).unwrap(),
            b"Shell fixture"
        );
    }

    #[test]
    fn shell_transfer_runs_file_actions_and_honors_cancellation() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("Datei.txt");
        let target = root.join("Ziel");
        fs::write(&source, b"test data").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let run = |source: &Path, action| {
            let report = transfer(
                &[source.to_path_buf()],
                &action,
                ShellTransferOptions::default(),
                cancel.clone(),
                |_| {},
                true,
            )
            .unwrap();
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.completed, 1);
        };
        run(root, ShellAction::NewFolder("Ziel".into()));
        run(&source, ShellAction::Copy(target.clone()));
        let copied = target.join("Datei.txt");
        assert_eq!(fs::read(&copied).unwrap(), b"test data");
        run(&copied, ShellAction::Rename("Kopie.txt".into()));
        assert!(target.join("Kopie.txt").exists());
        run(&source, ShellAction::Move(target.clone()));
        assert!(!source.exists());
        assert_eq!(fs::read(&copied).unwrap(), b"test data");
        cancel.store(true, Ordering::Relaxed);
        let report = transfer(
            std::slice::from_ref(&copied),
            &ShellAction::Delete,
            ShellTransferOptions::default(),
            cancel.clone(),
            |_| {},
            true,
        )
        .unwrap();
        assert!(report.cancelled);
        assert!(copied.exists());
        cancel.store(false, Ordering::Relaxed);
        run(&copied, ShellAction::Delete);
        assert!(!copied.exists());
    }
}
