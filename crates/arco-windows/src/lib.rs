//! Adapts Windows Shell functions for the desktop application.

pub mod ftp;

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Default)]
pub struct MetadataEdit {
    pub set: u32,
    pub clear: u32,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

#[cfg(not(windows))]
pub fn edit_metadata(_: &Path, _: MetadataEdit) -> io::Result<()> {
    Err(unsupported())
}

pub struct DiskSpace {
    pub available: u64,
    pub total: u64,
}

pub struct FileAllocation {
    pub bytes: u64,
    pub hardlink_id: Option<(u64, [u8; 16])>,
}

#[derive(Clone, Copy)]
pub enum KnownFolder {
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    Profile,
}

#[derive(Clone, Debug)]
pub struct ShellFolder {
    pub name: String,
    pub parsing_name: String,
    pub path: Option<PathBuf>,
    pub computer: bool,
    pub directory: bool,
}

pub const SHELL_DESKTOP: &str = "shell:Desktop";
pub const SHELL_COMPUTER: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

pub struct ShellListing {
    pub ancestors: Vec<(String, String)>,
    pub entries: Vec<ShellFolder>,
}

#[cfg(windows)]
pub use shell_items::{open_shell_item, shell_folders, shell_listing};

#[cfg(not(windows))]
pub fn shell_folders(_: Option<&str>, _: bool) -> io::Result<Vec<ShellFolder>> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn open_shell_item(_: &str) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn shell_listing(
    _: &str,
    _: bool,
    _: &std::sync::atomic::AtomicBool,
) -> io::Result<ShellListing> {
    Err(unsupported())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalShare {
    pub name: String,
    pub path: PathBuf,
    pub special: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Volume {
    pub root: PathBuf,
    pub label: String,
    pub identifier: PathBuf,
}

#[cfg(windows)]
mod administration;
#[cfg(windows)]
pub use administration::{create_share, local_shares, remove_share, rename_volume, volume};

#[cfg(not(windows))]
pub fn local_shares() -> io::Result<Vec<LocalShare>> {
    Err(unsupported())
}
#[cfg(not(windows))]
pub fn create_share(_: &Path, _: &str, _: bool) -> io::Result<()> {
    Err(unsupported())
}
#[cfg(not(windows))]
pub fn remove_share(_: &LocalShare) -> io::Result<()> {
    Err(unsupported())
}
#[cfg(not(windows))]
pub fn volume(_: &Path) -> io::Result<Volume> {
    Err(unsupported())
}
#[cfg(not(windows))]
pub fn rename_volume(_: &Volume, _: &str) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(windows)]
mod locations;
#[cfg(windows)]
pub use locations::{
    known_folder, network_drive_dialog, tc_favorite_folder, total_commander_install,
};

#[cfg(not(windows))]
pub fn tc_favorite_folder(_: &str) -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn total_commander_install() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
mod shell_actions;
#[cfg(windows)]
pub use shell_actions::{
    create_shortcut, execute_verb, parse_windows_arguments, replace_preserving_attributes,
};

#[cfg(not(windows))]
pub fn parse_windows_arguments(_: &str) -> io::Result<Vec<String>> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn create_shortcut(_: &Path, _: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn execute_verb(_: &Path, _: &str) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn replace_preserving_attributes(staged: &Path, target: &Path, _: &Path) -> io::Result<()> {
    std::fs::rename(staged, target)
}

#[cfg(not(windows))]
pub fn known_folder(_: KnownFolder) -> io::Result<PathBuf> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn network_drive_dialog(_: bool) -> io::Result<bool> {
    Err(unsupported())
}

#[cfg(windows)]
mod context_menu;
#[cfg(windows)]
pub mod credentials;
#[cfg(windows)]
mod drag;
#[cfg(windows)]
mod embedded;
#[cfg(windows)]
mod keyboard;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod printing;
#[cfg(windows)]
pub use printing::print_text;

#[cfg(not(windows))]
pub fn print_text(_: &str, _: &str) -> io::Result<bool> {
    Err(unsupported())
}
#[cfg(windows)]
pub mod sfx;
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
pub fn system_information(_: bool) -> io::Result<()> {
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
pub fn paste_text() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows-Zwischenablage erforderlich.",
    ))
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
