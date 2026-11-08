#![deny(unsafe_code)]

mod ace;
mod ace_lz77;
pub mod advanced_search;
mod archive_parts;
pub mod archive_view;
mod archive_zone;
pub mod archives;
pub mod descriptions;
pub mod everything;
pub mod external_archives;
pub mod files;
pub mod ignore;
pub mod legacy_archives;
#[allow(unsafe_code)] // Win32 file information, security and sparse-file APIs.
mod native_copy;
pub mod packer_profile;
pub mod plugin;
pub mod profile;
pub mod rename;
mod seven_zip;
pub mod sync;
pub mod tc_split;
pub mod text_compare;
pub mod tools;
pub mod transport_encoding;
mod uc2_volumes;
pub mod view_settings;
