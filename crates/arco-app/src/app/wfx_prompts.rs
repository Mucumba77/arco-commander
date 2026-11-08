use super::{App, Event, JobContext};
use crate::commands::{Dialog, Pending};
use arco_core::plugin::{Prompt, PromptAnswer, PromptKind};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, RecvTimeoutError, Sender},
};
use std::time::{Duration, Instant};

pub(crate) struct PromptTicket {
    name: String,
    prompt: Prompt,
    answer: Sender<PromptAnswer>,
    cancel: Arc<AtomicBool>,
    expired: Arc<AtomicBool>,
}

impl PromptTicket {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.expired.load(Ordering::Relaxed)
    }

    pub(crate) fn answer(self, value: &str) {
        if !self.cancelled() && value.len() <= 1024 {
            let _ = self.answer.send(PromptAnswer {
                value: value.to_owned(),
            });
        }
    }
}

impl JobContext {
    pub(super) fn prompt(&self, name: &str, prompt: Prompt) -> Option<PromptAnswer> {
        let (answer, response) = mpsc::channel();
        let expired = Arc::new(AtomicBool::new(false));
        self.sender
            .send(Event::PluginPrompt(PromptTicket {
                name: name.into(),
                prompt,
                answer,
                cancel: self.cancel.clone(),
                expired: expired.clone(),
            }))
            .ok()?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(300) && !self.cancel.load(Ordering::Relaxed) {
            match response.recv_timeout(Duration::from_millis(50)) {
                Ok(answer) => return Some(answer),
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) => (),
            }
        }
        expired.store(true, Ordering::Relaxed);
        None
    }
}

impl App {
    pub(super) fn open_next_plugin_prompt(&mut self) {
        if matches!(&self.pending, Some(Pending::WfxPrompt(ticket)) if ticket.cancelled()) {
            self.close_dialog();
        }
        let ui = self.ui();
        if !ui.get_dialog_kind().is_empty()
            || ui.get_settings_open()
            || ui.get_drive_side() >= 0
            || self.context_menu_open
        {
            return;
        }
        self.plugin_prompts.retain(|ticket| !ticket.cancelled());
        let Some(ticket) = self.plugin_prompts.pop_front() else {
            return;
        };
        let title = format!("WFX · {}", ticket.name);
        let description = format!("{}\n{}", ticket.prompt.title, ticket.prompt.text);
        let secret = ticket.prompt.kind == PromptKind::Secret;
        let input = matches!(ticket.prompt.kind, PromptKind::Text | PromptKind::Secret);
        self.dialog(
            if input { "input" } else { "confirm" },
            Pending::WfxPrompt(ticket),
            Dialog {
                title: &title,
                description: &description,
                placeholder: if secret {
                    "Passwort"
                } else if input {
                    "Antwort"
                } else {
                    ""
                },
                secret,
                ..Dialog::default()
            },
        );
    }
}
