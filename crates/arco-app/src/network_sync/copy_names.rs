use super::FtpEndpoint;
use arco_core::{
    sync::Tree,
    transfer_names::{NameRules, remote_leaf},
};
use arco_windows::ftp;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

const WINDOWS_NAMES: NameRules = NameRules {
    case: arco_core::transfer_names::NameCase::Preserve,
    windows: true,
};

pub(super) struct CopyNames {
    source: usize,
    rules: NameRules,
    original: BTreeMap<PathBuf, PathBuf>,
}

impl CopyNames {
    pub(super) fn new(source: usize, rules: NameRules) -> Self {
        Self {
            source,
            rules,
            original: BTreeMap::new(),
        }
    }

    pub(super) fn source(&self) -> usize {
        self.source
    }

    pub(super) fn selected_source(&self, path: &Path) -> io::Result<PathBuf> {
        if self.source == 1 && self.rules.windows {
            let name = path
                .to_str()
                .ok_or_else(|| io::Error::other("Dateinamen benötigen Unicode."))?;
            return encode_remote_name(name).map(PathBuf::from);
        }
        Ok(path.to_path_buf())
    }

    pub(super) fn destination(&self, path: &Path) -> io::Result<PathBuf> {
        let mut output = PathBuf::new();
        for segment in path.iter() {
            let name = segment
                .to_str()
                .ok_or_else(|| io::Error::other("Dateinamen benötigen Unicode."))?;
            let target = if self.source == 0 {
                let raw = self.rules.upload(name)?;
                if self.rules.windows {
                    encode_remote_name(&raw)?
                } else {
                    raw
                }
            } else {
                let raw = if self.rules.windows {
                    decode_remote_name(name)?
                } else {
                    name.to_owned()
                };
                self.rules.download(&raw)?
            };
            output.push(target);
        }
        Ok(output)
    }

    pub(super) fn remap(&mut self, tree: &mut Tree) -> io::Result<()> {
        self.original = tree.remap_paths(|path| self.destination(path))?;
        Ok(())
    }

    pub(super) fn original<'a>(&'a self, side: usize, path: &'a Path) -> &'a Path {
        if side == self.source {
            self.original.get(path).map_or(path, PathBuf::as_path)
        } else {
            path
        }
    }

    pub(super) fn original_paths(&self) -> &BTreeMap<PathBuf, PathBuf> {
        &self.original
    }
}

pub fn encode_remote_name(name: &str) -> io::Result<String> {
    WINDOWS_NAMES.download(name)
}

pub fn decode_remote_name(name: &str) -> io::Result<String> {
    let name = WINDOWS_NAMES.upload(name)?;
    remote_leaf(&name)?;
    Ok(name)
}

fn decode_path_component(name: &std::ffi::OsStr) -> io::Result<String> {
    let name = name
        .to_str()
        .ok_or_else(|| io::Error::other("FTP-Pfade benötigen Unicode."))?;
    decode_remote_name(name)
}

pub fn remote_parent(endpoint: &FtpEndpoint, path: &Path, escaped: bool) -> io::Result<ftp::Url> {
    if !escaped {
        return super::remote_parent(endpoint, path);
    }
    let (session, directory) = endpoint;
    let mut directory = directory.clone();
    if let Some(parent) = path.parent() {
        for name in parent.iter() {
            directory = session.child_directory(&directory, &decode_path_component(name)?)?;
        }
    }
    Ok(directory)
}

pub fn reported_path(path: &Path, escaped: bool) -> io::Result<PathBuf> {
    if !escaped {
        return Ok(path.to_path_buf());
    }
    let names = path
        .iter()
        .map(decode_path_component)
        .collect::<io::Result<Vec<_>>>()?;
    Ok(PathBuf::from(names.join("/")))
}
