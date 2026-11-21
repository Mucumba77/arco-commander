use super::{
    Request, Security, Session, Url, config_value, invalid_input, read_limited, run_observed,
};
use base64::Engine as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

pub(super) struct HttpRequest<'a> {
    session: &'a Session,
    logical_url: Url,
    url: Url,
    verb: &'static str,
    config: Zeroizing<String>,
    upload: Option<PathBuf>,
    timeout: bool,
}

pub(super) struct Response {
    pub url: Url,
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
            401 | 403 | 407 | 423 => io::ErrorKind::PermissionDenied,
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

    pub(super) fn http(&self, url: &Url, verb: &'static str) -> io::Result<HttpRequest<'_>> {
        self.validate(url)?;
        let forward = self.is_http_forward_ftp();
        if forward && verb != "GET" {
            return Err(super::forward::read_only());
        }
        if !forward && !matches!(self.security(), Security::WebDav | Security::S3) {
            return Err(invalid_input("Dieser Auftrag benötigt WebDAV oder S3."));
        }
        let wire = if forward {
            self.connection.options.encode_path(url)?
        } else if self.security() == Security::S3 {
            self.connection.s3_wire_url(url)?
        } else {
            self.webdav_wire_url(url)?
        };
        let mut request = HttpRequest {
            session: self,
            logical_url: url.clone(),
            url: wire,
            verb,
            config: Zeroizing::default(),
            upload: None,
            timeout: false,
        };
        if self.security() == Security::S3
            && self.connection.s3_requester_pays
            && !url.path().trim_matches('/').is_empty()
        {
            request.header("x-amz-request-payer", "requester")?;
        }
        if self.security() == Security::WebDav && verb == "GET" {
            request.header("Translate", "f")?;
        }
        Ok(request)
    }

    fn http_command(&self, wire: &Url, verb: &str) -> io::Result<(Command, Zeroizing<String>)> {
        let forward = self.is_http_forward_ftp();
        let same_webdav_origin =
            self.security() != Security::WebDav || wire.origin() == self.connection.url.origin();
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
            if forward { "=ftp" } else { "=http,https" },
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
            command
                .arg(if forward {
                    "--proxy-cacert"
                } else {
                    "--cacert"
                })
                .arg(path);
        }
        if same_webdav_origin && let Some(cert) = &self.connection.options.client_certificate {
            command.args(["--cert", &format!("CurrentUser\\MY\\{cert}")]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut config = Zeroizing::default();
        self.connection.append_proxy_config(&mut config);
        if self.security() == Security::S3 {
            command.args([
                "--aws-sigv4",
                &format!("aws:amz:{}:s3", self.connection.s3_region),
            ]);
        } else if forward && !self.connection.user.is_empty() {
            // libcurl puts --user in the FTP request URL when the HTTP proxy
            // is not tunneled. Keep credentials out of the request target.
            let credentials = Zeroizing::new(format!(
                "{}:{}",
                self.connection.user,
                self.password.as_str()
            ));
            let encoded = Zeroizing::new(
                base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes()),
            );
            let authorization = Zeroizing::new(format!("Basic {}", encoded.as_str()));
            append_header(&mut config, "Authorization", &authorization)?;
        } else if same_webdav_origin && self.connection.webdav_windows_auth {
            command.arg("--negotiate");
            append_credentials(&mut config, "", "");
        } else if same_webdav_origin && !self.connection.user.is_empty() {
            command.arg("--anyauth");
            append_credentials(&mut config, &self.connection.user, &self.password);
        }
        Ok((command, config))
    }
}

impl HttpRequest<'_> {
    pub(super) fn wire_url(&self) -> &Url {
        &self.url
    }

    pub(super) fn logical_url(&self) -> &Url {
        &self.logical_url
    }

    pub(super) fn receive_file(
        self,
        output: &File,
        limit: Option<u64>,
        cancel: &AtomicBool,
        observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Response> {
        let response = self
            .execute_limited(Some(output.try_clone()?), limit, cancel, observe)?
            .expect(&[200])?;
        if let Some(limit) = limit
            && output.metadata()?.len() > limit
        {
            return Err(invalid_input(
                "Die HTTP-Datei überschreitet das Größenlimit.",
            ));
        }
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
        append_credentials(&mut self.config, user, password);
    }

    pub fn header(&mut self, key: &str, value: &str) -> io::Result<&mut Self> {
        append_header(&mut self.config, key, value)?;
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
        self.upload = Some(file.to_owned());
        self
    }

    pub fn send(mut self, cancel: &AtomicBool) -> io::Result<Response> {
        self.timeout = true;
        self.execute(None, cancel, || Ok(()))
    }

    pub fn execute(
        self,
        output: Option<File>,
        cancel: &AtomicBool,
        observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Response> {
        self.execute_limited(output, None, cancel, observe)
    }

    fn execute_limited(
        mut self,
        mut output: Option<File>,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Response> {
        let webdav = self.session.security() == Security::WebDav;
        let mut visited = BTreeSet::new();
        loop {
            let mut receiving_file = false;
            let response = self.execute_once(output.as_ref(), limit, cancel, |headers| {
                if let Some(output) = &output {
                    let received = output.metadata()?.len();
                    if webdav && received > 0 && !receiving_file {
                        receiving_file = last_http_status(headers)? == Some(200);
                    }
                    // Report the requested file only. Bound bodies of redirects and errors independently.
                    if webdav && !receiving_file {
                        return if received <= 1024 * 1024 {
                            Ok(())
                        } else {
                            Err(invalid_input("Die WebDAV-Zwischenantwort ist zu groß."))
                        };
                    }
                    if limit.is_some_and(|limit| received > limit) {
                        return Err(invalid_input(
                            "Die HTTP-Datei überschreitet das Größenlimit.",
                        ));
                    }
                }
                observe()
            })?;
            if !webdav || !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let target =
                self.session
                    .webdav_redirect_target(&self.logical_url, self.verb, &response)?;
            if !visited.insert(self.url.clone()) || visited.contains(&target) || visited.len() > 8 {
                return Err(invalid_input(
                    "Die WebDAV-Weiterleitung bildet eine Schleife oder ist zu lang.",
                ));
            }
            if let Some(file) = &mut output {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
            }
            self.url = target;
        }
    }

    fn execute_once(
        &mut self,
        output: Option<&File>,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut observe: impl FnMut(&NamedTempFile) -> io::Result<()>,
    ) -> io::Result<Response> {
        use std::fmt::Write as _;
        let (mut command, mut config) = self.session.http_command(&self.url, self.verb)?;
        if self.session.security() == Security::S3 {
            self.session.sign_s3_request(self, cancel)?;
            if let Some(limit) = limit {
                command.args(["--max-filesize", &limit.max(1).to_string()]);
            }
        }
        if let Some(upload) = &self.upload {
            command.arg("--upload-file").arg(upload);
        }
        if self.timeout {
            command.args(["--max-time", "75"]);
        }
        let headers = NamedTempFile::new()?;
        command.arg("--dump-header").arg(headers.path());
        config.push_str(&self.config);
        writeln!(config, "url = {}", config_value(self.url.as_str())).expect("format HTTP URL");
        let body = run_observed(
            Request {
                command,
                config: Zeroizing::new(config.as_bytes().to_vec()),
                encoding: encoding_rs::UTF_8,
                native_ftp: None,
                rate: matches!(self.verb, "GET" | "PUT").then(|| self.session.transfer_rate()),
                protocol: "HTTP",
            },
            &[],
            output.map(File::try_clone).transpose()?,
            cancel,
            || {
                if headers.as_file().metadata()?.len() > 256 * 1024 {
                    return Err(invalid_input("Die HTTP-Antwort enthält zu viele Header."));
                }
                observe(&headers)
            },
        )?;
        let data = read_limited(headers.reopen()?, 256 * 1024)?;
        let text = String::from_utf8_lossy(&data);
        let mut response = Response {
            url: self.url.clone(),
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
                let name = name.to_ascii_lowercase();
                if self.session.security() == Security::WebDav
                    && name == "location"
                    && response.headers.contains_key(&name)
                    && matches!(response.status, 301 | 302 | 303 | 307 | 308)
                {
                    return Err(invalid_input("Ungültiges Ziel der WebDAV-Weiterleitung."));
                }
                response.headers.insert(name, value.trim().to_owned());
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

fn append_credentials(config: &mut String, user: &str, password: &str) {
    use std::fmt::Write as _;
    let credential = Zeroizing::new(format!("{user}:{password}"));
    let quoted = Zeroizing::new(config_value(&credential));
    writeln!(config, "user = {}", quoted.as_str()).expect("format curl credentials");
}

fn append_header(config: &mut String, key: &str, value: &str) -> io::Result<()> {
    use std::fmt::Write as _;
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    {
        return Err(invalid_input("Ungültiger HTTP-Headername."));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_input("Ungültiger HTTP-Headerwert."));
    }
    let header = Zeroizing::new(format!("{key}: {value}"));
    let quoted = Zeroizing::new(config_value(&header));
    writeln!(config, "header = {}", quoted.as_str()).expect("format curl header");
    Ok(())
}

fn last_http_status(headers: &NamedTempFile) -> io::Result<Option<u16>> {
    let mut status = None;
    for line in BufReader::new(headers.reopen()?).split(b'\n') {
        let line = line?;
        if line.starts_with(b"HTTP/") {
            status = String::from_utf8_lossy(&line)
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse().ok());
        }
    }
    Ok(status)
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
