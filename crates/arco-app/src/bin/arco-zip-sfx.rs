#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use arco_core::archives::{ZIP_SFX_PASSWORD_LIMIT, extract_zip_sfx, zip_sfx_encrypted};
    use std::{
        io::{self, Read},
        path::PathBuf,
        process::ExitCode,
        sync::atomic::AtomicBool,
    };
    use zeroize::Zeroizing;

    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let interactive = first.is_none();
    let extract = || -> io::Result<Option<usize>> {
        let target = if interactive {
            arco_windows::sfx::destination()?
        } else if first.is_some_and(|arg| arg == "--extract-to") {
            let target = args
                .next()
                .filter(|_| args.next().is_none())
                .ok_or_else(|| io::Error::other("Verwendung: Archiv.exe --extract-to <Ordner>"))?;
            Some(PathBuf::from(target))
        } else {
            return Err(io::Error::other("Unbekanntes Argument."));
        };
        let Some(target) = target else {
            return Ok(None);
        };
        let archive = std::env::current_exe()?;
        let cancel = AtomicBool::new(false);
        let password = if zip_sfx_encrypted(&archive, &cancel)? {
            if interactive {
                let Some(password) = arco_windows::sfx::password(ZIP_SFX_PASSWORD_LIMIT)? else {
                    return Ok(None);
                };
                Some(password)
            } else {
                let mut password = Zeroizing::new(String::new());
                let max_bytes = (ZIP_SFX_PASSWORD_LIMIT * 4) as u64;
                io::stdin()
                    .lock()
                    .take(max_bytes + 1)
                    .read_to_string(&mut password)?;
                if password.len() as u64 > max_bytes {
                    return Err(io::Error::other("Das ZIP-SFX-Passwort ist zu lang."));
                }
                Some(password)
            }
        } else {
            None
        };
        let password = password.as_ref().map(|password| password.as_str());
        extract_zip_sfx(&archive, &target, password, &cancel).map(Some)
    };
    match extract() {
        Ok(count) => {
            if interactive && let Some(count) = count {
                arco_windows::sfx::notice(&format!("{count} Dateien entpackt."), false);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if interactive {
                arco_windows::sfx::notice(&error.to_string(), true);
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
