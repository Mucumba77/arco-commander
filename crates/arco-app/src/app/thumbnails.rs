use super::{App, Event};
use crate::panel::FileModel;
use arco_core::{
    files,
    plugin::{self, Interface},
};
use std::cell::OnceCell;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{ops::Range, rc::Rc};

pub(super) struct Loaded {
    side: usize,
    visible: Range<usize>,
    images: Vec<(usize, Option<image::RgbaImage>)>,
}

pub(super) struct Running {
    model: Rc<FileModel>,
    visible: Range<usize>,
    cancel: Arc<AtomicBool>,
}

impl App {
    fn thumbnails_visible(&self, side: usize) -> bool {
        let panel = &self.panels[side];
        !panel.loading
            && panel.current().shows_thumbnails()
            && (!self.quick_open || side == self.active)
    }

    pub(super) fn update_thumbnails(&mut self) {
        let ui = self.ui();
        for side in 0..2 {
            let shown = self.thumbnails_visible(side);
            let (first, count) = if side == 0 {
                (ui.get_left_wdx_first(), ui.get_left_wdx_count())
            } else {
                (ui.get_right_wdx_first(), ui.get_right_wdx_count())
            };
            let first = first.max(0) as usize;
            let visible = first..first.saturating_add(count.clamp(1, 96) as usize + 1);
            let model = self.panels[side].current().file_model();
            if let Some(running) = &self.thumbnail_jobs[side] {
                if !shown
                    || !Rc::ptr_eq(&running.model, &model)
                    || visible.start >= running.visible.end
                    || running.visible.start >= visible.end
                {
                    running.cancel.store(true, Ordering::Relaxed);
                }
                continue;
            }
            if !shown {
                continue;
            }
            let compiled = OnceCell::new();
            let plugins = || {
                compiled.get_or_init(|| {
                    self.profile
                        .plugins
                        .iter()
                        .filter(|entry| {
                            entry.enabled
                                && entry.interface == Interface::Wlx
                                && !entry.masks.is_empty()
                        })
                        .filter_map(|entry| {
                            Some((entry.path.clone(), files::compile_masks(&entry.masks).ok()?))
                        })
                        .collect::<Vec<_>>()
                })
            };
            let requests = model.thumbnail_requests(visible.clone(), |path| {
                path.file_name()
                    .is_some_and(|name| plugins().iter().any(|(_, masks)| masks.is_match(name)))
            });
            if requests.is_empty() {
                continue;
            }
            plugins();
            let plugins = compiled.into_inner().unwrap_or_default();
            let cancel = Arc::new(AtomicBool::new(false));
            self.thumbnail_jobs[side] = Some(Running {
                model,
                visible: visible.clone(),
                cancel: cancel.clone(),
            });
            let sender = self.sender.clone();
            std::thread::spawn(move || {
                let mut images: Vec<_> = requests
                    .iter()
                    .take_while(|_| !cancel.load(Ordering::Relaxed))
                    .map(|(row, request)| (*row, request.load()))
                    .collect();
                for (plugin, masks) in plugins {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let pending: Vec<_> = requests
                        .iter()
                        .zip(&images)
                        .enumerate()
                        .filter_map(|(index, ((_, request), (_, image)))| {
                            let path = request.path();
                            (image.is_none()
                                && path.file_name().is_some_and(|name| masks.is_match(name)))
                            .then(|| Some((index, request.snapshot()?)))
                            .flatten()
                        })
                        .collect();
                    if pending.is_empty() {
                        continue;
                    }
                    let paths = pending
                        .iter()
                        .map(|(index, _)| requests[*index].1.path().to_path_buf())
                        .collect();
                    if let Ok(bitmaps) = plugin::wlx_thumbnails(&plugin, paths, &cancel) {
                        for (index, bitmap) in bitmaps {
                            let (row, before) = &pending[index];
                            if files::ensure_unchanged(requests[*row].1.path(), before).is_ok() {
                                images[*row].1 = image::RgbaImage::from_raw(
                                    bitmap.width,
                                    bitmap.height,
                                    bitmap.rgba,
                                );
                            }
                        }
                    }
                }
                let _ = sender.send(Event::Thumbnails(Loaded {
                    side,
                    visible,
                    images,
                }));
            });
        }
    }

    pub(super) fn apply_thumbnails(&mut self, loaded: Loaded) {
        let Some(running) = self.thumbnail_jobs[loaded.side].take() else {
            return;
        };
        if self.thumbnails_visible(loaded.side)
            && !running.cancel.load(Ordering::Relaxed)
            && Rc::ptr_eq(
                &running.model,
                &self.panels[loaded.side].current().file_model(),
            )
        {
            running
                .model
                .apply_thumbnails(loaded.visible, loaded.images);
        }
    }

    pub(super) fn invalidate_thumbnails(&self) {
        for running in self.thumbnail_jobs.iter().flatten() {
            running.cancel.store(true, Ordering::Relaxed);
        }
        for tab in self.panels.iter().flat_map(|panel| &panel.tabs) {
            tab.file_model().clear_thumbnails();
        }
    }
}
