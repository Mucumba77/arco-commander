use std::io;

/// TC view-mode lifecycle. `-` suppresses tab switches, `*` also runs when
/// navigating inside the same mode, and `|` separates entry/exit commands.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewCommands {
    pub on_enter: Vec<String>,
    pub on_leave: Vec<String>,
    pub every_directory: bool,
    pub suppress_tabs: bool,
}

impl ViewCommands {
    pub fn parse(value: &str) -> io::Result<Self> {
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(invalid());
        }
        let mut result = Self::default();
        let mut rest = value.trim();
        while let Some(prefix) = rest.chars().next().filter(|c| *c == '-' || *c == '*') {
            let flag = if prefix == '-' {
                &mut result.suppress_tabs
            } else {
                &mut result.every_directory
            };
            if *flag {
                return Err(invalid());
            }
            *flag = true;
            rest = rest[1..].trim_start();
        }
        let (enter, leave) = rest.split_once('|').unwrap_or((rest, ""));
        let parse = |text: &str| -> io::Result<Vec<String>> {
            if text.contains('|') {
                return Err(invalid());
            }
            let commands: Vec<_> = text
                .split(',')
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .collect();
            if commands.len() > 32 {
                return Err(invalid());
            }
            for command in &commands {
                let verb = command
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !verb.starts_with("cm_")
                    && !verb.starts_with("em_")
                    && !verb.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(invalid());
                }
            }
            Ok(commands)
        };
        result.on_enter = parse(enter)?;
        result.on_leave = parse(leave)?;
        Ok(result)
    }

    pub fn entering(
        &self,
        mode_changed: bool,
        directory_changed: bool,
        tab_changed: bool,
    ) -> &[String] {
        if !(self.suppress_tabs && tab_changed)
            && (mode_changed || self.every_directory && directory_changed)
        {
            &self.on_enter
        } else {
            &[]
        }
    }
    pub fn leaving(&self, mode_changed: bool, tab_changed: bool) -> &[String] {
        if mode_changed && !(self.suppress_tabs && tab_changed) {
            &self.on_leave
        } else {
            &[]
        }
    }
}

fn invalid() -> io::Error {
    io::Error::other(
        "Ungültige automatische Ansichtsbefehle. Interne Befehle mit Komma trennen; hinter | stehen Befehle beim Verlassen.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn view_lifecycle_obeys_entry_exit_repetition_and_tab_suppression() {
        let commands =
            ViewCommands::parse("-*cm_SrcAllFiles, cm_SwitchIgnoreList 1|cm_50Percent").unwrap();
        assert_eq!(commands.entering(true, true, false).len(), 2);
        assert_eq!(commands.entering(false, true, false).len(), 2);
        assert!(commands.entering(false, false, false).is_empty());
        assert!(commands.entering(true, true, true).is_empty());
        assert_eq!(commands.leaving(true, false), ["cm_50Percent"]);
        assert!(commands.leaving(false, false).is_empty());
        assert!(commands.leaving(true, true).is_empty());
        let ordinary = ViewCommands::parse("cm_SrcAllFiles").unwrap();
        assert!(ordinary.entering(false, true, false).is_empty());
        for input in [
            "cm_Open|cm_Close|cm_Copy",
            "**cm_Copy",
            "calc.exe",
            "cm_Copy\ncm_Delete",
        ] {
            assert!(ViewCommands::parse(input).is_err());
        }
    }
}
