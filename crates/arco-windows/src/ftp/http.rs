use super::{
    Request, Security, Session, Url, config_value, invalid_input, read_limited, run_observed,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

pub(super) struct HttpRequest<'a> {
    url: Url,
    command: Command,
    config: Zeroizing<String>,
    s3: Option<&'a Session>,
}

pub(super) struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn expect(self, accepted: &[u16]) -> io::Result<Self> {
        if accepted.contains(&self.status) {
            return Ok(self);
        }
        let kind = match self.status {
            401 | 403 | 423 => io::ErrorKind::PermissionDenied,
            404 => io::ErrorKind::NotFound,
            405 | 501 => io::ErrorKind::Unsupported,
            409 | 412 => io::ErrorKind::AlreadyExists,
            _ => io::ErrorKind::Other,
        };
        let message = if self.status == 423 {
            "Die Serverressource ist gesperrt.".into()
        } else {
            format!("Der Server meldet HTTP {}.", self.status)
        };
        Err(io::Error::new(kind, message))
    }
}

impl Session {
    pub(super) fn receive_http(
        &self,
        url: &Url,
        output: &File,
        limit: Option<u64>,
        cancel: &AtomicBool,
        observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        self.http(url, "GET")?
            .receive_file(output, limit, cancel, observe)
            .map(|_| ())
    }

    pub(super) fn http(&self, url: &Url, verb: &str) -> io::Result<HttpRequest<'_>> {
        self.validate(url)?;
        if !matches!(self.security(), Security::WebDav | Security::S3) {
            return Err(invalid_input("Dieser Auftrag benötigt WebDAV oder S3."));
        }
        let root =
            std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot fehlt."))?;
        let mut command = Command::new(PathBuf::from(root).join("System32").join("curl.exe"));
        command.args([
            "--disable",
            "--config",
            "-",
            "--silent",
            "--show-error",
            "--globoff",
            "--proto",
            "=http,https",
            "--connect-timeout",
            "15",
            "--speed-time",
            "30",
            "--speed-limit",
            "1",
        ]);
        if verb == "HEAD" {
            command.arg("--head");
        }
        command.args(["--request", verb]);
        if matches!(verb, "GET" | "PUT") {
            self.apply_curl_rate(&mut command);
        }
        if let Some(proxy) = &self.connection.proxy {
            command.args(["--noproxy", "", "--proxy", proxy.as_str()]);
        } else {
            command.args(["--noproxy", "*"]);
        }
        match self.connection.options.family {
            super::AddressFamily::Automatic => (),
            super::AddressFamily::Ipv4 => {
                command.arg("--ipv4");
            }
            super::AddressFamily::Ipv6 => {
                command.arg("--ipv6");
            }
        }
        if let Some(path) = &self.connection.options.ca_bundle {
            command.arg("--cacert").arg(path);
        }
        if let Some(cert) = &self.connection.options.client_certificate {
            command.args(["--cert", &format!("CurrentUser\\MY\\{cert}")]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut request = HttpRequest {
            url: url.clone(),
            command,
            config: Zeroizing::default(),
            s3: (self.security() == Security::S3).then_some(self),
        };
        self.connection.append_proxy_config(&mut request.config);
        if self.security() == Security::S3 {
            request.command.args([
                "--aws-sigv4",
                &format!("aws:amz:{}:s3", self.connection.s3_region),
            ]);
        } else if !self.connection.user.is_empty() {
            request.command.arg("--anyauth");
            request.credentials(&self.connection.user, &self.password);
        }
        Ok(request)
    }
}

impl HttpRequest<'_> {
    pub(super) fn receive_file(
        mut self,
        output: &File,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Response> {
        // For WebDAV, curl applies --max-filesize to the authentication
        // challenge body as well. Bound the actual output stream instead.
        if self.s3.is_some()
            && let Some(limit) = limit
        {
            self.command
                .args(["--max-filesize", &limit.max(1).to_string()]);
        }
        let response = self
            .execute(Some(output.try_clone()?), cancel, || {
                if let Some(limit) = limit
                    && output.metadata()?.len() > limit
                {
                    return Err(invalid_input(
                        "Die HTTP-Datei überschreitet das Größenlimit.",
                    ));
                }
                observe()
            })?
            .expect(&[200])?;
        if let Some(length) = response
            .headers
            .get("content-length")
            .and_then(|value| value.parse::<u64>().ok())
            && output.metadata()?.len() != length
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Die Serverdatei hat eine unerwartete Größe.",
            ));
        }
        Ok(response)
    }

    pub(super) fn credentials(&mut self, user: &str, password: &str) {
        use std::fmt::Write as _;
        let credential = Zeroizing::new(format!("{user}:{password}"));
        let quoted = Zeroizing::new(config_value(&credential));
        writeln!(self.config, "user = {}", quoted.as_str()).expect("format curl credentials");
    }

    pub fn header(&mut self, key: &str, value: &str) -> io::Result<&mut Self> {
        use std::fmt::Write as _;
        if value.chars().any(char::is_control) {
            return Err(invalid_input("Ungültiger HTTP-Headerwert."));
        }
        let header = Zeroizing::new(format!("{key}: {value}"));
        writeln!(self.config, "header = {}", config_value(&header)).expect("format curl header");
        Ok(self)
    }

    pub fn body(&mut self, data: &str) -> io::Result<&mut Self> {
        use std::fmt::Write as _;
        if data.starts_with('@') || data.chars().any(char::is_control) {
            return Err(invalid_input("Ungültiger HTTP-Anfragetext."));
        }
        writeln!(self.config, "data-binary = {}", config_value(data)).expect("format curl body");
        Ok(self)
    }

    pub fn query(&mut self, name: &str, value: &str) -> &mut Self {
        // S3 SigV4 requires percent-encoded spaces, including in object prefixes.
        use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
        const QUERY: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC
            .remove(b'-')
            .remove(b'_')
            .remove(b'.')
            .remove(b'~');
        let mut query = self.url.query().unwrap_or_default().to_owned();
        if !query.is_empty() {
            query.push('&');
        }
        use std::fmt::Write as _;
        write!(
            query,
            "{}={}",
            percent_encode(name.as_bytes(), QUERY),
            percent_encode(value.as_bytes(), QUERY)
        )
        .expect("format query");
        self.url.set_query(Some(&query));
        self
    }

    pub fn upload(&mut self, file: &Path) -> &mut Self {
        self.command.arg("--upload-file").arg(file);
        self
    }

    pub fn send(mut self, cancel: &AtomicBool) -> io::Result<Response> {
        self.command.args(["--max-time", "75"]);
        self.execute(None, cancel, || Ok(()))
    }

    pub fn execute(
        mut self,
        output: Option<File>,
        cancel: &AtomicBool,
        mut observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Response> {
        use std::fmt::Write as _;
        if let Some(session) = self.s3 {
            session.sign_s3_request(&mut self, cancel)?;
        }
        let headers = NamedTempFile::new()?;
        self.command.arg("--dump-header").arg(headers.path());
        writeln!(self.config, "url = {}", config_value(self.url.as_str()))
            .expect("format HTTP URL");
        let body = run_observed(
            Request {
                command: self.command,
                config: Zeroizing::new(self.config.as_bytes().to_vec()),
                encoding: encoding_rs::UTF_8,
                otp: None,
                protocol: "HTTP",
            },
            &[],
            output,
            cancel,
            || {
                if headers.as_file().metadata()?.len() > 256 * 1024 {
                    return Err(invalid_input("Die HTTP-Antwort enthält zu viele Header."));
                }
                observe()
            },
        )?;
        let data = read_limited(headers.reopen()?, 256 * 1024)?;
        let text = String::from_utf8_lossy(&data);
        let mut response = Response {
            status: 0,
            headers: BTreeMap::new(),
            body,
        };
        for line in text.lines() {
            if line.starts_with("HTTP/") {
                response.status = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|code| code.parse().ok())
                    .unwrap_or(0);
                response.headers.clear();
            } else if let Some((name, value)) = line.split_once(':') {
                response
                    .headers
                    .insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        if response.status == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Der Server hat keine HTTP-Antwort geliefert.",
            ));
        }
        Ok(response)
    }
}

pub(super) fn xml(body: &[u8]) -> io::Result<roxmltree::Document<'_>> {
    let text = std::str::from_utf8(body).map_err(io::Error::other)?;
    roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 400_000,
            ..Default::default()
        },
    )
    .map_err(io::Error::other)
}
