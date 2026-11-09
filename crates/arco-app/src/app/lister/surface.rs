use crate::preview::Rich;
use std::io;
use std::path::Path;

#[cfg(windows)]
mod windows {
    use super::*;
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::raw_window_handle::{
        HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
    };
    use std::borrow::Cow;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS, ICoreWebView2_3,
    };
    use windows_core::{HSTRING, Interface, w};
    use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

    const VIEW_URL: &str = "https://arco-lister.localhost/";
    const MEDIA_HOST: &str = "arco-media.invalid";
    const PLAYER_KEYS: &str = r#"<script nonce="arco-player">
        const player = document.querySelector("audio, video");
        document.addEventListener("keydown", event => {
            if (event.altKey || event.metaKey) return;
            if (event.key.toLowerCase() === "m" && !event.ctrlKey) {
                if (!event.repeat) player.muted = !player.muted;
            } else if (event.key === "ArrowUp" || event.key === "ArrowDown") {
                const step = event.shiftKey || event.ctrlKey ? .10 : .01;
                const volume = player.volume + (event.key === "ArrowUp" ? step : -step);
                player.volume = Math.min(1, Math.max(0, volume));
            } else {
                return;
            }
            event.preventDefault();
        }, true);
    </script>"#;

    struct Parent(Win32WindowHandle);

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            // Surface::new keeps the shown Slint window alive during construction.
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(self.0)) })
        }
    }

    pub(crate) struct Surface {
        webview: WebView,
        _profile: tempfile::TempDir,
    }

    impl Surface {
        pub(crate) fn new(
            window: &slint::Window,
            path: &Path,
            preview: Rich,
            light: bool,
            embedded: bool,
        ) -> io::Result<Self> {
            let (parent, initial) = window
                .with_winit_window(|native| {
                    let RawWindowHandle::Win32(handle) = native.window_handle().ok()?.as_raw()
                    else {
                        return None;
                    };
                    Some((Parent(handle), bounds(native)))
                })
                .flatten()
                .ok_or_else(|| io::Error::other("Kein Windows-Fenster für den Lister."))?;
            let (media_path, html) = document(path, preview, light)?;
            let profile = tempfile::tempdir()?;
            let mut context = wry::WebContext::new(Some(profile.path().to_path_buf()));
            let webview = WebViewBuilder::new_with_web_context(&mut context)
                .with_https_scheme(true)
                .with_incognito(true)
                .with_visible(false)
                .with_autoplay(false)
                .with_browser_accelerator_keys(false)
                .with_bounds(if embedded {
                    wry::Rect::default()
                } else {
                    initial
                })
                .with_navigation_handler(|url| url == VIEW_URL || url == "about:blank")
                .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
                .with_custom_protocol("arco-lister".into(), move |_, request| {
                    use wry::http::{Method, Response, StatusCode};
                    if request.method() == Method::GET
                        && request.uri().scheme_str() == Some("arco-lister")
                        && request.uri().host() == Some("localhost")
                        && request
                            .uri()
                            .path_and_query()
                            .is_some_and(|path| path.as_str() == "/")
                    {
                        Response::builder()
                            .header("Content-Type", "text/html; charset=utf-8")
                            .header("X-Content-Type-Options", "nosniff")
                            .body(Cow::Owned(html.as_bytes().to_vec()))
                            .expect("static preview headers")
                    } else {
                        let mut response = Response::new(Cow::Borrowed(b"".as_slice()));
                        *response.status_mut() = StatusCode::NOT_FOUND;
                        response
                    }
                })
                .build_as_child(&parent)
                .map_err(|error| {
                    io::Error::other(format!("WebView2 konnte nicht gestartet werden. {error}"))
                })?;
            let core = unsafe { webview.controller().CoreWebView2() }.map_err(io::Error::other)?;
            let settings = unsafe { core.Settings() }.map_err(io::Error::other)?;
            let configure = || unsafe {
                settings.SetIsScriptEnabled(media_path.is_some())?;
                settings.SetIsWebMessageEnabled(false)?;
                settings.SetAreHostObjectsAllowed(false)?;
                settings.SetAreDevToolsEnabled(false)?;
                settings.SetAreDefaultContextMenusEnabled(false)?;
                settings.SetAreDefaultScriptDialogsEnabled(false)
            };
            configure().map_err(io::Error::other)?;
            if let Some(parent) = media_path {
                let parent = HSTRING::from(parent.as_os_str());
                let mapping: ICoreWebView2_3 = core.cast().map_err(io::Error::other)?;
                unsafe {
                    mapping.SetVirtualHostNameToFolderMapping(
                        w!("arco-media.invalid"),
                        &parent,
                        COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS,
                    )
                }
                .map_err(io::Error::other)?;
            }
            webview.load_url(VIEW_URL).map_err(io::Error::other)?;
            if !embedded {
                webview.set_visible(true).map_err(io::Error::other)?;
            }
            Ok(Self {
                webview,
                _profile: profile,
            })
        }

        pub(crate) fn resize(&self, window: &slint::Window) {
            if let Some(bounds) = window.with_winit_window(bounds) {
                let _ = self.webview.set_bounds(bounds);
            }
        }

        pub(crate) fn place(&self, rect: Option<[i32; 4]>) -> io::Result<()> {
            if let Some([x, y, width, height]) = rect {
                self.webview
                    .set_bounds(wry::Rect {
                        position: wry::dpi::PhysicalPosition::new(x, y).into(),
                        size: wry::dpi::PhysicalSize::new(
                            width.max(1) as u32,
                            height.max(1) as u32,
                        )
                        .into(),
                    })
                    .map_err(io::Error::other)?;
            }
            self.webview
                .set_visible(rect.is_some())
                .map_err(io::Error::other)
        }
    }

    fn bounds(window: &slint::winit_030::winit::window::Window) -> wry::Rect {
        let size = window.inner_size();
        let controls = (60.0 * window.scale_factor()).ceil() as u32;
        wry::Rect {
            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
            size: wry::dpi::PhysicalSize::new(
                size.width,
                size.height.saturating_sub(controls).max(1),
            )
            .into(),
        }
    }

    fn document(path: &Path, preview: Rich, light: bool) -> io::Result<(Option<&Path>, String)> {
        let mut media = None;
        let mut media_rule = String::new();
        let body = match preview {
            Rich::Html(html) | Rich::Rtf(html) => html,
            Rich::Media { element, mime } => {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        io::Error::other("Der Mediendateiname ist nicht in Unicode darstellbar.")
                    })?;
                media = path.parent();
                let url = format!(
                    "https://{MEDIA_HOST}/{}",
                    utf8_percent_encode(name, NON_ALPHANUMERIC)
                );
                media_rule = format!("; media-src {url}; script-src 'nonce-arco-player'");
                format!(
                    r#"<main class="player"><{element} controls preload="metadata" controlslist="nodownload noremoteplayback"><source src="{url}" type="{mime}"></{element}><p>{}</p></main>{PLAYER_KEYS}"#,
                    ammonia::clean_text(name)
                )
            }
        };
        let scheme = if light { "light" } else { "dark" };
        let html = format!(
            r#"<!doctype html><html lang="de"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'{media_rule}"><style>
            :root {{ color-scheme: {scheme}; font: 16px 'Segoe UI', sans-serif; }}
            body {{ margin: 18px; overflow-wrap: anywhere; background: Canvas; color: CanvasText; }}
            img, table {{ max-width: 100%; }} pre {{ white-space: pre-wrap; }} td, th {{ padding: .25rem .5rem; }}
            .rtf {{ white-space: pre-wrap; tab-size: 4; }} .player {{ display: grid; place-items: center; min-height: 80vh; }}
            .rtf-table {{ border-collapse: collapse; }} .rtf-table td {{ border: 1px solid GrayText; vertical-align: top; }}
            .rtf-list {{ list-style: none; margin: 0; padding: 0; }}
            .rtf-list > li {{ display: grid; grid-template-columns: max-content minmax(0,1fr); column-gap: .5em; }}
            video {{ width: 100%; max-height: 72vh; }} audio {{ width: min(100%, 36rem); }}
            </style></head><body>{body}</body></html>"#,
        );
        Ok((media, html))
    }

    #[cfg(test)]
    #[test]
    fn only_the_media_document_allows_the_fixed_player_script() {
        let path = Path::new(r"C:\Videos\a & b.mp4");
        let (_, rich) = document(path, Rich::Html("<p>document</p>".into()), true).unwrap();
        assert!(!rich.contains("script-src"));
        let (_, media) = document(
            path,
            Rich::Media {
                element: "video",
                mime: "video/mp4",
            },
            false,
        )
        .unwrap();
        assert!(media.contains("script-src 'nonce-arco-player'"));
        assert!(media.contains("nonce=\"arco-player\""));
        assert!(media.contains("a%20%26%20b%2Emp4"));
        assert!(media.contains("a&#32;&amp;&#32;b.mp4"));
    }
}

#[cfg(windows)]
pub(in crate::app) use windows::Surface;

#[cfg(not(windows))]
pub(in crate::app) struct Surface;

#[cfg(not(windows))]
impl Surface {
    pub(in crate::app) fn new(
        _: &slint::Window,
        _: &Path,
        _: Rich,
        _: bool,
        _: bool,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Die formatierte Vorschau benötigt Windows und WebView2.",
        ))
    }

    pub(in crate::app) fn place(&self, _: Option<[i32; 4]>) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}
