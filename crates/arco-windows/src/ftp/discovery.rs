use super::{Connection, Security, Url, check_cancel, invalid_input};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io;
use std::net::IpAddr;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};
use windows_core::PCWSTR;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, GlobalFree};
use windows_sys::Win32::Networking::WinHttp::*;

#[derive(Debug, PartialEq, Eq)]
pub enum HttpProxy {
    Address(Url),
    Direct,
}

struct Target {
    url: Url,
    allow_https_proxy: bool,
}

impl Target {
    fn new(address: &str, security: Security) -> io::Result<Self> {
        let remote = Connection::new(address, "arco", security)?.url;
        let url = match security {
            Security::WebDav | Security::S3 => remote,
            _ => {
                let scheme = if security == Security::Plain {
                    "http"
                } else {
                    "https"
                };
                let host = remote.host_str().expect("validated server host");
                let port = remote.port().unwrap_or(match security {
                    Security::Plain | Security::ExplicitTls => 21,
                    Security::ImplicitTls => 990,
                    _ => 22,
                });
                Url::parse(&format!("{scheme}://{host}:{port}/")).map_err(io::Error::other)?
            }
        };
        Ok(Self {
            url,
            allow_https_proxy: !security.is_ssh(),
        })
    }

    fn proxy(&self, address: &str) -> Option<Url> {
        let address = if address.contains("://") {
            Url::parse(address).ok()?
        } else {
            Url::parse(&format!("http://{address}")).ok()?
        };
        (address.host().is_some()
            && (address.scheme() == "http"
                || self.allow_https_proxy && address.scheme() == "https")
            && address.username().is_empty()
            && address.password().is_none()
            && address.path() == "/"
            && address.query().is_none()
            && address.fragment().is_none())
        .then_some(address)
    }
}

pub fn detect_http_proxy(
    address: &str,
    security: Security,
    cancel: &AtomicBool,
) -> io::Result<HttpProxy> {
    check_cancel(cancel)?;
    let target = Target::new(address, security)?;
    let mut config = UserSettings(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default());
    let mut auto_failure = None;
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config.0) } != 0 {
        let pac = wide_text(config.0.lpszAutoConfigUrl)?;
        for source in [
            (!pac.is_empty()).then_some(Some(pac.as_str())),
            (config.0.fAutoDetect != 0).then_some(None),
        ]
        .into_iter()
        .flatten()
        {
            match auto_proxy(&target, source, cancel) {
                Ok(route) => return Ok(route),
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted
                        || error.kind() == io::ErrorKind::Unsupported =>
                {
                    return Err(error);
                }
                Err(error)
                    if error.raw_os_error() == Some(ERROR_WINHTTP_AUTODETECTION_FAILED as i32) => {}
                Err(error) => auto_failure = Some(error),
            }
        }
        let manual = wide_text(config.0.lpszProxy)?;
        let bypass = wide_text(config.0.lpszProxyBypass)?;
        if let Some(route) = configured_proxy(&target, &manual, &bypass)? {
            return Ok(route);
        }
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_FILE_NOT_FOUND as i32) {
            return Err(error);
        }
    }
    let mut machine = MachineSettings(WINHTTP_PROXY_INFO::default());
    if unsafe { WinHttpGetDefaultProxyConfiguration(&mut machine.0) } != 0
        && machine.0.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY
    {
        let manual = wide_text(machine.0.lpszProxy)?;
        let bypass = wide_text(machine.0.lpszProxyBypass)?;
        if let Some(route) = configured_proxy(&target, &manual, &bypass)? {
            return Ok(route);
        }
    }
    if let Some(error) = auto_failure {
        return Err(io::Error::new(
            error.kind(),
            format!("Windows konnte die automatische Proxy-Konfiguration nicht auswerten. {error}"),
        ));
    }
    Ok(HttpProxy::Direct)
}

fn configured_proxy(target: &Target, list: &str, bypass: &str) -> io::Result<Option<HttpProxy>> {
    let mut named = Vec::new();
    let mut defaults = Vec::new();
    for spec in words(list) {
        if let Some((scheme, proxy)) = spec.split_once('=') {
            if scheme.eq_ignore_ascii_case(target.url.scheme()) {
                named.push(proxy);
            }
        } else {
            defaults.push(spec);
        }
    }
    let applicable = if named.is_empty() { defaults } else { named };
    if applicable.is_empty() {
        return Ok(None);
    }
    if words(bypass).any(|pattern| bypasses(&target.url, pattern)) {
        return Ok(Some(HttpProxy::Direct));
    }
    applicable
        .into_iter()
        .find_map(|proxy| target.proxy(proxy))
        .map(|proxy| Some(HttpProxy::Address(proxy)))
        .ok_or_else(unsupported)
}

fn words(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| c == ';' || c.is_whitespace())
        .filter(|word| !word.is_empty())
}

fn bypasses(target: &Url, rule: &str) -> bool {
    let host = target.host_str().expect("validated HTTP target");
    if rule.eq_ignore_ascii_case("<local>") {
        return !host.contains('.') && !host.contains(':');
    }
    let pattern = if let Some((scheme, pattern)) = rule.split_once("://") {
        if !scheme.eq_ignore_ascii_case(target.scheme()) {
            return false;
        }
        pattern.trim_end_matches('/')
    } else {
        rule
    };
    let port = target.port_or_known_default().expect("HTTP default port");
    let host_port = format!("{host}:{port}");
    let pattern = regex::escape(pattern)
        .replace("\\*", ".*")
        .replace("\\?", ".");
    regex::Regex::new(&format!("(?i)^{pattern}$"))
        .is_ok_and(|pattern| pattern.is_match(host) || pattern.is_match(&host_port))
}

fn auto_proxy(target: &Target, pac: Option<&str>, cancel: &AtomicBool) -> io::Result<HttpProxy> {
    check_cancel(cancel)?;
    let endpoint = wide(target.url.as_str())?;
    let pac = pac.map(wide).transpose()?;
    let agent = wide("Arco Commander")?;
    let session = Handle(unsafe {
        WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            null(),
            null(),
            WINHTTP_FLAG_ASYNC,
        )
    });
    if session.0.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut raw = null_mut();
    win_result(unsafe { WinHttpCreateProxyResolver(session.0, &mut raw) })?;
    let resolver = Handle(raw);
    let callback = unsafe {
        WinHttpSetStatusCallback(
            resolver.0,
            Some(completed),
            WINHTTP_CALLBACK_FLAG_GETPROXYFORURL_COMPLETE | WINHTTP_CALLBACK_FLAG_REQUEST_ERROR,
            0,
        )
    };
    if callback.is_some_and(|previous| previous as usize == usize::MAX) {
        return Err(io::Error::last_os_error());
    }
    let options = WINHTTP_AUTOPROXY_OPTIONS {
        dwFlags: if pac.is_some() {
            WINHTTP_AUTOPROXY_CONFIG_URL
        } else {
            WINHTTP_AUTOPROXY_AUTO_DETECT
        },
        dwAutoDetectFlags: if pac.is_some() {
            0
        } else {
            WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A
        },
        lpszAutoConfigUrl: pac.as_ref().map_or(null(), |path| path.as_ptr()),
        ..Default::default()
    };
    let (send, receive) = mpsc::sync_channel(1);
    let request = Request(NEXT_REQUEST.fetch_add(1, Ordering::Relaxed));
    callbacks()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(request.0, send);
    let status =
        unsafe { WinHttpGetProxyForUrlEx(resolver.0, endpoint.as_ptr(), &options, request.0) };
    if status == ERROR_IO_PENDING {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            check_cancel(cancel)?;
            match receive.recv_timeout(Duration::from_millis(75)) {
                Ok(status) => {
                    win_result(status)?;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Zeitlimit für die Windows-Proxy-Erkennung erreicht.",
                    ));
                }
            }
        }
    } else {
        win_result(status)?;
    }
    check_cancel(cancel)?;
    let mut result = AutoResult(WINHTTP_PROXY_RESULT::default());
    win_result(unsafe { WinHttpGetProxyResult(resolver.0, &mut result.0) })?;
    if result.0.cEntries == 0 || result.0.pEntries.is_null() {
        return Ok(HttpProxy::Direct);
    }
    let routes =
        unsafe { std::slice::from_raw_parts(result.0.pEntries, result.0.cEntries as usize) };
    for route in routes {
        if route.fProxy == 0 {
            return Ok(HttpProxy::Direct);
        }
        let scheme = match route.ProxyScheme {
            WINHTTP_INTERNET_SCHEME_HTTP => "http",
            WINHTTP_INTERNET_SCHEME_HTTPS => "https",
            _ => continue,
        };
        let mut host = wide_text(route.pwszProxy)?;
        if host.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
            host = format!("[{host}]");
        }
        let address = if route.ProxyPort == 0 {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}:{}", route.ProxyPort)
        };
        if let Some(proxy) = target.proxy(&address) {
            return Ok(HttpProxy::Address(proxy));
        }
    }
    Err(unsupported())
}

static NEXT_REQUEST: AtomicUsize = AtomicUsize::new(1);
type Callbacks = Mutex<BTreeMap<usize, mpsc::SyncSender<u32>>>;

fn callbacks() -> &'static Callbacks {
    static REGISTRY: std::sync::OnceLock<Callbacks> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(Callbacks::default)
}

unsafe extern "system" fn completed(
    _: *mut c_void,
    context: usize,
    status: u32,
    info: *mut c_void,
    length: u32,
) {
    let result = match status {
        WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE => 0,
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
            if !info.is_null()
                && length as usize >= std::mem::size_of::<WINHTTP_ASYNC_RESULT>() =>
        {
            // WinHTTP owns this result for the duration of the callback.
            unsafe { (*info.cast::<WINHTTP_ASYNC_RESULT>()).dwError }
        }
        _ => return,
    };
    if let Ok(registry) = callbacks().lock()
        && let Some(sender) = registry.get(&context)
    {
        let _ = sender.try_send(result);
    }
}

struct Request(usize);
impl Drop for Request {
    fn drop(&mut self) {
        // Late WinHTTP callbacks only receive the id and can no longer reach the job.
        callbacks()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.0);
    }
}

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

struct UserSettings(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG);
impl Drop for UserSettings {
    fn drop(&mut self) {
        for value in [
            self.0.lpszAutoConfigUrl,
            self.0.lpszProxy,
            self.0.lpszProxyBypass,
        ] {
            if !value.is_null() {
                unsafe {
                    GlobalFree(value.cast());
                }
            }
        }
    }
}

struct MachineSettings(WINHTTP_PROXY_INFO);
impl Drop for MachineSettings {
    fn drop(&mut self) {
        for value in [self.0.lpszProxy, self.0.lpszProxyBypass] {
            if !value.is_null() {
                unsafe {
                    GlobalFree(value.cast());
                }
            }
        }
    }
}

struct AutoResult(WINHTTP_PROXY_RESULT);
impl Drop for AutoResult {
    fn drop(&mut self) {
        if !self.0.pEntries.is_null() {
            unsafe {
                WinHttpFreeProxyResult(&mut self.0);
            }
        }
    }
}

fn wide(value: &str) -> io::Result<Vec<u16>> {
    if value.contains('\0') {
        return Err(invalid_input(
            "Ungültige Adresse für die Windows-Proxy-Erkennung.",
        ));
    }
    Ok(value.encode_utf16().chain(std::iter::once(0)).collect())
}

fn wide_text(value: *mut u16) -> io::Result<String> {
    if value.is_null() {
        return Ok(String::new());
    }
    unsafe { PCWSTR(value).to_string() }.map_err(io::Error::other)
}

fn win_result(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows hat für dieses Protokoll keinen verwendbaren HTTP-Proxy angegeben.",
    )
}

#[cfg(test)]
mod tests;
