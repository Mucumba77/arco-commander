use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread::JoinHandle;

#[test]
fn configured_windows_proxy_respects_target_scheme_and_bypass_list() {
    let target = Target::new("sftp://FILES.EXAMPLE.test:2222/projects", Security::Sftp).unwrap();
    assert_eq!(target.url.as_str(), "https://files.example.test:2222/");
    let list = "http=web-proxy.test:81; https=secure-proxy.test:82; generic.test:83";
    assert_eq!(
        configured_proxy(&target, list, "other.test").unwrap(),
        Some(HttpProxy::Address(
            Url::parse("http://secure-proxy.test:82").unwrap()
        ))
    );
    assert_eq!(
        configured_proxy(&target, list, "<local>; https://*.example.test:2222").unwrap(),
        Some(HttpProxy::Direct)
    );
    let local = Target::new("ftp://intranet/", Security::Plain).unwrap();
    assert_eq!(
        configured_proxy(&local, list, "<local>").unwrap(),
        Some(HttpProxy::Direct)
    );
    let dav = Target::new("http://webdav.test:8080/root", Security::WebDav).unwrap();
    assert_eq!(dav.url.as_str(), "http://webdav.test:8080/root/");
    assert_eq!(
        configured_proxy(&dav, "http=https://[::1]:8443", "").unwrap(),
        Some(HttpProxy::Address(
            Url::parse("https://[::1]:8443").unwrap()
        ))
    );
    assert!(configured_proxy(&target, "https=https://proxy.test:8443", "").is_err());
    assert!(configured_proxy(&target, "https=http://account:secret@proxy.test:8443", "").is_err());
    assert!(Target::new("sftp://user:secret@files.example.test/", Security::Sftp).is_err());
}

struct PacServer {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl PacServer {
    fn start(body: Option<&'static str>) -> (Self, mpsc::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/arco.pac", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let halt = stop.clone();
        let (accept, accepted) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            while !halt.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = String::new();
                        let mut reader = BufReader::new(&mut stream);
                        while reader.read_line(&mut request).unwrap_or(0) != 0 {
                            if request.ends_with("\r\n\r\n") {
                                break;
                            }
                        }
                        let _ = accept.try_send(());
                        if let Some(body) = body {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        } else {
                            while !halt.load(Ordering::Relaxed) {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        (
            Self {
                url,
                stop,
                worker: Some(worker),
            },
            accepted,
        )
    }
}

impl Drop for PacServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
#[ignore = "requires the Windows WinHTTP proxy service and loopback"]
fn reference_winhttp_evaluates_pac_routes_and_cancels_pending_requests() {
    let (pac, _) = PacServer::start(Some(
        r#"
        function FindProxyForURL(url, host) {
            if (host == "direct.test") return "DIRECT; PROXY backup.test:8182";
            return "PROXY first.test:8181; PROXY backup.test:8182; DIRECT";
        }
    "#,
    ));
    let cancel = AtomicBool::new(false);
    let named = Target::new("sftp://files.example.test:2222/", Security::Sftp).unwrap();
    assert_eq!(
        auto_proxy(&named, Some(&pac.url), &cancel).unwrap(),
        HttpProxy::Address(Url::parse("http://first.test:8181").unwrap())
    );
    let direct = Target::new("http://direct.test/resource", Security::WebDav).unwrap();
    assert_eq!(
        auto_proxy(&direct, Some(&pac.url), &cancel).unwrap(),
        HttpProxy::Direct
    );
    let (stall, accepted) = PacServer::start(None);
    std::thread::scope(|scope| {
        let cancel = &cancel;
        let interrupt = scope.spawn(move || {
            let arrived = accepted.recv_timeout(Duration::from_secs(4)).is_ok();
            cancel.store(true, Ordering::Relaxed);
            arrived
        });
        let start = Instant::now();
        let error = auto_proxy(&named, Some(&stall.url), cancel).unwrap_err();
        assert!(interrupt.join().unwrap());
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(start.elapsed() < Duration::from_secs(7));
    });
}
