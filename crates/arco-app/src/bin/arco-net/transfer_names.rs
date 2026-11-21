use super::invalid;
use arco_core::transfer_names::{NameCase, NameRules};
use arco_windows::ftp::Session;
use std::io;
use std::iter::Peekable;
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
pub(super) struct NameOverrides {
    case: Option<NameCase>,
    windows: Option<bool>,
}

impl NameOverrides {
    pub(super) fn parse<'a>(
        &mut self,
        arg: &str,
        rest: &mut Peekable<impl Iterator<Item = &'a str>>,
    ) -> io::Result<bool> {
        if let Some(case) = arg.strip_prefix("-namecase=") {
            self.case = Some(NameCase::parse(case).ok_or_else(|| {
                invalid("-namecase erwartet preserve, lower, upper, first-upper oder lower-dos.")
            })?);
        } else if matches!(arg, "-windowsnames" | "-nowindowsnames") {
            self.windows = Some(arg == "-windowsnames");
        } else if arg == "-rawtransfersettings" {
            if !rest.peek().is_some_and(is_assignment) {
                return Err(invalid(
                    "-rawtransfersettings erwartet mindestens eine Einstellung im Format Name=Wert.",
                ));
            }
            while rest.peek().is_some_and(is_assignment) {
                let setting = rest.next().expect("peeked assignment");
                let (name, value) = setting.split_once('=').expect("parsed assignment");
                self.set_raw(name, value)?;
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn set_raw(&mut self, name: &str, value: &str) -> io::Result<()> {
        if name.eq_ignore_ascii_case("FileNameCase") {
            self.case = Some(match value {
                "0" => NameCase::Preserve,
                "1" => NameCase::Upper,
                "2" => NameCase::Lower,
                "4" => NameCase::LowerDos,
                _ => return Err(invalid("FileNameCase erwartet 0, 1, 2 oder 4.")),
            });
        } else if name.eq_ignore_ascii_case("ReplaceInvalidChars") {
            self.windows = Some(match value {
                "0" => false,
                "1" => true,
                _ => return Err(invalid("ReplaceInvalidChars erwartet 0 oder 1.")),
            });
        } else {
            return Err(invalid("Unbekannte Einstellung für -rawtransfersettings."));
        }
        Ok(())
    }

    pub(super) fn is_empty(self) -> bool {
        self.case.is_none() && self.windows.is_none()
    }

    pub(super) fn apply(self, names: &mut NameRules) {
        names.case = self.case.unwrap_or(names.case);
        names.windows = self.windows.unwrap_or(names.windows);
    }

    pub(super) fn session(self, session: Arc<Session>) -> Arc<Session> {
        if self.is_empty() {
            return session;
        }
        let mut settings = session.transfer_settings().clone();
        self.apply(&mut settings.names);
        Arc::new(session.with_transfer_snapshot(settings))
    }
}

fn is_assignment(arg: &&str) -> bool {
    arg.split_once('=')
        .is_some_and(|(key, _)| !key.is_empty() && key.bytes().all(|ch| ch.is_ascii_alphabetic()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_winscp_assignments_until_the_next_argument() {
        let mut options = NameOverrides::default();
        let mut arguments = ["FileNameCase=1", "ReplaceInvalidChars=1", "--", "a=b.txt"]
            .into_iter()
            .peekable();
        assert!(
            options
                .parse("-rawtransfersettings", &mut arguments)
                .unwrap()
        );
        let mut names = NameRules::default();
        options.apply(&mut names);
        assert_eq!(names.case, NameCase::Upper);
        assert!(names.windows);
        assert_eq!(arguments.next(), Some("--"));
        assert_eq!(arguments.next(), Some("a=b.txt"));
    }

    #[test]
    fn rejects_unsupported_assignments_and_replacement_codes() {
        for setting in [
            "PreserveTimeDirs=1",
            "ReplaceInvalidChars=95",
            "FileNameCase=8",
        ] {
            let mut arguments = [setting].into_iter().peekable();
            assert!(
                NameOverrides::default()
                    .parse("-rawtransfersettings", &mut arguments)
                    .is_err()
            );
        }
    }
}
