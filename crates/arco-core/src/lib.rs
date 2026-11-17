#![deny(unsafe_code)]

mod ace;
mod ace_lz77;
mod ace_volumes;
pub mod advanced_search;
mod archive_parts;
mod archive_rewrite;
pub mod archive_settings;
#[cfg(test)]
mod archive_tests;
pub mod archive_view;
mod archive_zone;
pub mod archives;
mod compressed_tar;
pub mod descriptions;
pub mod everything;
pub mod external_archives;
mod external_volumes;
pub mod files;
pub mod ignore;
pub mod io_progress;
pub mod legacy_archives;
#[allow(unsafe_code)] // Win32 file information, security and sparse-file APIs.
mod native_copy;
pub mod pack_selection;
pub mod packer_profile;
pub mod plugin;
pub mod profile;
mod read_archives;
pub mod rename;
mod seven_zip;
pub mod sync;
pub mod tc_split;
pub mod text_compare;
pub mod tools;
pub mod transport_encoding;
mod uc2_volumes;
pub mod view_settings;
