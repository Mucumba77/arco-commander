//! Adapts Windows Shell functions for the desktop application.

pub mod ftp;

use std::io;
use std::path::{Path, PathBuf};

pub struct DiskSpace {
    pub available: u64,
    pub total: u64,
}

pub struct FileAllocation {
    pub bytes: u64,
    pub hardlink_id: Option<(u64, [u8; 16])>,
}

#[cfg(windows)]
mod context_menu;
#[cfg(windows)]
mod drag;
#[cfg(windows)]
mod embedded;
#[cfg(windows)]
mod keyboard;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod shell_items;

#[cfg(windows)]
pub use context_menu::show_context_menu;
#[cfg(windows)]
pub use drag::drag_copy;
#[cfg(windows)]
pub use embedded::{close_embedded, position_embedded};
#[cfg(windows)]
pub use keyboard::{NativeShortcut, ShortcutHook};
#[cfg(windows)]
pub use platform::*;

#[cfg(not(windows))]
pub fn attach_parent_console_for_help() {}

#[cfg(not(windows))]
pub fn open(_: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn properties(_: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn show_context_menu(_: &[PathBuf], _: (i32, i32), _: bool) -> io::Result<bool> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn is_double_click(_: (i32, i32), _: (i32, i32), _: std::time::Duration) -> bool {
    false
}

#[cfg(not(windows))]
pub fn edit(_: &Path, _: Option<&Path>) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn terminal(_: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn recycle(_: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn rename_noreplace(_: &Path, _: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn drives() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

#[cfg(not(windows))]
pub fn disk_space(_: &Path) -> Option<DiskSpace> {
    None
}

#[cfg(not(windows))]
pub fn file_allocation(_: &Path) -> io::Result<FileAllocation> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn copy_text(_: &str) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn copy_paths(_: &[PathBuf], _: bool) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn paste_paths() -> io::Result<(Vec<PathBuf>, bool)> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn cursor_screen() -> Option<(i32, i32)> {
    None
}

#[cfg(not(windows))]
pub fn drag_copy(_: &[PathBuf]) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "requires Windows Shell")
}
