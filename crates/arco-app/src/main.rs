#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod commands;
mod panel;
mod preview;
mod shortcuts;

use arco_core::profile::Profile;
use slint::ComponentHandle;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let mut paths = [None, None];
    let mut profile = None;
    let mut portable = false;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--help" | "-h" | "/?") => {
                arco_windows::attach_parent_console_for_help();
                println!(
                    "Arco Commander\n  --left PFAD | /L=PFAD\n  --right PFAD | /R=PFAD\n  --portable\n  --profile DATEI"
                );
                return Ok(());
            }
            Some("--left") => paths[0] = Some(next_path(&mut arguments, "--left")?),
            Some("--right") => paths[1] = Some(next_path(&mut arguments, "--right")?),
            Some("--portable") => portable = true,
            Some("--profile") => profile = Some(next_path(&mut arguments, "--profile")?),
            _ => {
                let value = argument.to_string_lossy();
                let (side, value) = value
                    .split_once('=')
                    .ok_or_else(|| format!("Unbekannte Option {value}"))?;
                match side.to_ascii_uppercase().as_str() {
                    "/L" => paths[0] = Some(value.into()),
                    "/R" => paths[1] = Some(value.into()),
                    _ => return Err(format!("Unbekannte Option {side}").into()),
                }
            }
        }
    }
    let fallback = std::env::current_dir()?;
    let profile = match profile {
        Some(path) => path,
        None if portable => std::env::current_exe()?.with_file_name("arco-commander.json"),
        None => std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| fallback.clone())
            .join("Arco Commander")
            .join("profile.json"),
    };
    let (mut saved, profile_error) = match Profile::load(&profile) {
        Ok(saved) => (saved, None),
        Err(error) => (Profile::default(), Some(error.to_string())),
    };
    for (panel, path) in saved.panels.iter_mut().zip(paths) {
        if let Some(path) = path {
            let path = if path.is_absolute() {
                path
            } else {
                fallback.join(path)
            };
            if panel.tabs.is_empty() {
                panel.tabs.push(path);
            } else {
                panel.active_tab = panel.active_tab.min(panel.tabs.len() - 1);
                panel.tabs[panel.active_tab] = path;
                if let Some(options) = panel.tab_options.get_mut(panel.active_tab) {
                    *options = Default::default();
                }
            }
        }
    }
    let window = MainWindow::new()?;
    window
        .window()
        .set_size(slint::LogicalSize::new(1200.0, 760.0));
    let _state = app::run(&window, saved, fallback, profile, profile_error);
    window.invoke_focus_panels();
    window.run()?;
    Ok(())
}

fn next_path(
    arguments: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("Pfad für {name} fehlt"))
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut amount = bytes as f64;
    for unit in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        amount /= 1024.0;
        if amount < 1024.0 {
            return format!("{amount:.1} {unit}");
        }
    }
    format!("{bytes} B")
}

fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
}
