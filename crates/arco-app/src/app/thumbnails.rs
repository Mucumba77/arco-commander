use super::{App, Event};
use crate::panel::FileModel;
use std::{ops::Range, rc::Rc};

pub(super) struct Loaded {
    side: usize,
    visible: Range<usize>,
    images: Vec<(usize, Option<image::RgbaImage>)>,
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
            if self.thumbnail_jobs[side].is_some() || !self.thumbnails_visible(side) {
                continue;
            }
            let (first, count) = if side == 0 {
                (ui.get_left_wdx_first(), ui.get_left_wdx_count())
            } else {
                (ui.get_right_wdx_first(), ui.get_right_wdx_count())
            };
            let first = first.max(0) as usize;
            let visible = first..first.saturating_add(count.clamp(1, 96) as usize + 1);
            let model = self.panels[side].current().file_model();
            let requests = model.thumbnail_requests(visible.clone());
            if requests.is_empty() {
                continue;
            }
            self.thumbnail_jobs[side] = Some(model);
            let sender = self.sender.clone();
            std::thread::spawn(move || {
                let images = requests
                    .into_iter()
                    .map(|(row, request)| (row, request.load()))
                    .collect();
                let _ = sender.send(Event::Thumbnails(Loaded {
                    side,
                    visible,
                    images,
                }));
            });
        }
    }

    pub(super) fn apply_thumbnails(&mut self, loaded: Loaded) {
        let Some(model): Option<Rc<FileModel>> = self.thumbnail_jobs[loaded.side].take() else {
            return;
        };
        if self.thumbnails_visible(loaded.side)
            && Rc::ptr_eq(&model, &self.panels[loaded.side].current().file_model())
        {
            model.apply_thumbnails(loaded.visible, loaded.images);
        }
    }
}
