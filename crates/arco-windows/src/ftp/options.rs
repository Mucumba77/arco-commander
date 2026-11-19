use super::{Session, Url, check_segment, invalid_input, run};
use encoding_rs::{Encoding, UTF_8};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, percent_encode};
use regex::Regex;
use std::borrow::Cow;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AddressFamily {
    #[default]
    Automatic,
    Ipv4,
    Ipv6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferMode {
    Binary,
    Text,
}

#[derive(Default)]
pub struct ConnectionOptions {
    pub family: AddressFamily,
    pub charset: String,
    pub list_pattern: String,
    pub show_hidden: bool,
    pub ca_bundle: Option<PathBuf>,
    pub client_certificate: Option<String>,
    pub mode_z: bool,
    pub otp: bool,
}

pub(super) struct Options {
    pub family: AddressFamily,
    pub encoding: &'static Encoding,
    pub list_pattern: Option<Regex>,
    pub show_hidden: bool,
    pub ca_bundle: Option<PathBuf>,
    pub client_certificate: Option<String>,
    pub mode_z: bool,
    pub otp: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            family: AddressFamily::Automatic,
            encoding: UTF_8,
            list_pattern: None,
            show_hidden: false,
            ca_bundle: None,
            client_certificate: None,
            mode_z: false,
            otp: false,
        }
    }
}

impl Options {
    pub fn new(input: ConnectionOptions) -> io::Result<Self> {
        let encoding = if input.charset.trim().is_empty() {
            UTF_8
        } else {
            Encoding::for_label(input.charset.trim().as_bytes())
                .filter(|encoding| encoding.is_ascii_compatible())
                .ok_or_else(|| invalid_input("FTP benötigt einen ASCII-kompatiblen Zeichensatz."))?
        };
        let list_pattern = if input.list_pattern.trim().is_empty() {
            None
        } else {
            if input.list_pattern.len() > 4096 {
                return Err(invalid_input("Der Ausdruck für FTP-LIST ist zu lang."));
            }
            let pattern = Regex::new(&format!(r"\A(?:{})\z", input.list_pattern))
                .map_err(io::Error::other)?;
            for name in ["name", "kind"] {
                if !pattern.capture_names().any(|capture| capture == Some(name)) {
                    return Err(invalid_input(
                        "Der LIST-Ausdruck benötigt die Gruppen name und kind.",
                    ));
                }
            }
            Some(pattern)
        };
        if input
            .ca_bundle
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || !path.is_file())
        {
            return Err(invalid_input(
                "Wähle einen absoluten Pfad zu einer vorhandenen PEM-CA-Datei.",
            ));
        }
        if input.client_certificate.as_ref().is_some_and(|thumbprint| {
            thumbprint.len() != 40 || !thumbprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(invalid_input(
                "Der Fingerabdruck des Clientzertifikats benötigt 40 Hex-Zeichen.",
            ));
        }
        Ok(Self {
            family: input.family,
            encoding,
            list_pattern,
            show_hidden: input.show_hidden,
            ca_bundle: input.ca_bundle,
            client_certificate: input.client_certificate,
            mode_z: input.mode_z,
            otp: input.otp,
        })
    }

    pub fn decode<'a>(&self, bytes: &'a [u8]) -> io::Result<Cow<'a, str>> {
        self.encoding
            .decode_without_bom_handling_and_without_replacement(bytes)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Die FTP-Daten passen nicht zum gewählten Zeichensatz.",
                )
            })
    }

    pub fn encode_path(&self, url: &Url) -> io::Result<Url> {
        let mut result = url.clone();
        let mut path = String::new();
        for segment in url
            .path()
            .strip_prefix('/')
            .unwrap_or(url.path())
            .split('/')
        {
            path.push('/');
            if segment.is_empty() {
                continue;
            }
            let decoded = percent_decode_str(segment)
                .decode_utf8()
                .map_err(io::Error::other)?;
            // curl interprets a leading %2F as the remote filesystem root.
            if decoded == "/" && path == "/" {
                path.push_str("%2F");
                continue;
            }
            check_segment(&decoded)?;
            let (bytes, _, errors) = self.encoding.encode(&decoded);
            if errors {
                return Err(invalid_input(
                    "Der FTP-Pfad ist im gewählten Zeichensatz nicht darstellbar.",
                ));
            }
            path.push_str(&percent_encode(&bytes, NON_ALPHANUMERIC).to_string());
        }
        result.set_path(&path);
        Ok(result)
    }
}

impl Session {
    pub fn connection_details(&self, directory: &Url, cancel: &AtomicBool) -> io::Result<String> {
        if self.is_sftp() {
            self.validate(directory)?;
            self.sftp_operation(cancel, |_| Ok(()))?;
            return Ok(format!("{directory}\nSSH · {}", self.sftp_fingerprint()?));
        }
        let response = self.server_command(directory, "FEAT", cancel)?;
        let options = &self.connection.options;
        Ok(format!(
            "{directory}\n{:?} · {:?} · {}\n\n{response}",
            self.connection.security,
            options.family,
            options.encoding.name()
        ))
    }

    pub fn download_location(&self, directory: &Url, value: &str) -> io::Result<(Url, String)> {
        if value.contains("://") {
            let mut url = Url::parse(value).map_err(io::Error::other)?;
            self.validate(&url)?;
            let name = url
                .path_segments()
                .and_then(Iterator::last)
                .ok_or_else(|| invalid_input("In der FTP-URL fehlt der Dateiname."))?;
            let name = percent_decode_str(name)
                .decode_utf8()
                .map_err(io::Error::other)?
                .into_owned();
            check_segment(&name)?;
            url.path_segments_mut()
                .map_err(|()| invalid_input("Ungültiger FTP-Pfad."))?
                .pop()
                .push("");
            self.connection.options.encode_path(&url)?;
            return Ok((url, name));
        }
        self.validate(directory)?;
        let (parent, name) = value.rsplit_once('/').unwrap_or(("", value));
        check_segment(name)?;
        let mut url = if value.starts_with('/') {
            self.directory("/")?
        } else {
            directory.clone()
        };
        for segment in parent
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
        {
            check_segment(segment)?;
            url = self.relative_directory(&url, std::path::Path::new(segment))?;
        }
        self.connection.options.encode_path(&url)?;
        Ok((url, name.to_owned()))
    }

    pub(super) fn raw_listing_bytes(
        &self,
        directory: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<u8>> {
        let arguments: &[&str] = if self.connection.options.show_hidden {
            &["--request", "LIST -a"]
        } else {
            &[]
        };
        run(self.command(directory)?, arguments, None, cancel)
    }

    pub fn raw_listing(&self, directory: &Url, cancel: &AtomicBool) -> io::Result<String> {
        let output = self.raw_listing_bytes(directory, cancel)?;
        Ok(self.connection.options.decode(&output)?.into_owned())
    }

    pub fn server_command(
        &self,
        directory: &Url,
        input: &str,
        cancel: &AtomicBool,
    ) -> io::Result<String> {
        let input = input.trim();
        let (verb, tail) = input.split_once(' ').unwrap_or((input, ""));
        let verb = verb.to_ascii_uppercase();
        let allowed = match verb.as_str() {
            "PWD" | "SYST" | "FEAT" | "NOOP" => tail.is_empty(),
            "STAT" | "HELP" | "SIZE" | "MDTM" | "SITE" | "OPTS" => true,
            _ => false,
        };
        if !allowed || input.len() > 1024 || input.chars().any(char::is_control) {
            return Err(invalid_input(
                "Unterstützt werden PWD, SYST, FEAT, NOOP, STAT, HELP, SIZE, MDTM, SITE und OPTS.",
            ));
        }
        let command = if tail.is_empty() {
            format!("+{verb}")
        } else {
            format!("+{verb} {tail}")
        };
        let response = run(
            self.command(directory)?,
            &[
                "--head",
                "--dump-header",
                "-",
                "--output",
                "NUL",
                "--max-time",
                "20",
                "--quote",
                &command,
            ],
            None,
            cancel,
        )?;
        let mut response = self.connection.options.decode(&response)?.into_owned();
        if !self.password.is_empty() {
            response = response.replace(self.password.as_str(), "••••");
        }
        Ok(response)
    }
}
