use super::{default_profile_path, invalid};
use arco_windows::task_schedule::{self, ScheduledScript, Trigger};
use serde_json::json;
use std::io::{IsTerminal, Read as _};
use std::{ffi::OsString, io, path::PathBuf};
use zeroize::Zeroizing;

pub(super) fn run(mut args: impl Iterator<Item = OsString>) -> Result<(), (u8, io::Error)> {
    let usage = || {
        (
            2,
            invalid("Ungültige Aufgabenoptionen. Verwende arco-net schedule --help."),
        )
    };
    let runtime = |error| (1, error);
    let verb = args.next();
    match verb.as_deref().and_then(std::ffi::OsStr::to_str) {
        None | Some("--help" | "-h") => {
            println!(
                "arco-net schedule create NAME ZEIT DATEI [--trigger daily|once|weekly|monthly|logon|startup] [--profile DATEI] [--log-dir ORDNER] [--background-password-stdin]\nZEIT: HH:MM, \"JJJJ-MM-TT HH:MM\", \"HH:MM Mo,Fr\", \"HH:MM 1,15\", logon oder startup\narco-net schedule list\narco-net schedule run NAME\narco-net schedule remove NAME"
            );
        }
        Some("create") => {
            let name = string_arg(args.next()).ok_or_else(usage)?;
            let at = string_arg(args.next()).ok_or_else(usage)?;
            let script = PathBuf::from(args.next().ok_or_else(usage)?);
            let mut profile = None;
            let mut log = None;
            let mut background = false;
            let mut trigger_kind = None;
            while let Some(option) = args.next() {
                if option == "--background-password-stdin" && !background {
                    background = true;
                    continue;
                }
                if option == "--trigger" && trigger_kind.is_none() {
                    trigger_kind = Some(string_arg(args.next()).ok_or_else(usage)?);
                    continue;
                }
                let slot = match option.to_str() {
                    Some("--profile") if profile.is_none() => &mut profile,
                    Some("--log-dir") if log.is_none() => &mut log,
                    _ => return Err(usage()),
                };
                *slot = Some(PathBuf::from(args.next().ok_or_else(usage)?));
            }
            let executable = std::env::current_exe().map_err(runtime)?;
            let profile = profile
                .map_or_else(default_profile_path, Ok)
                .map_err(runtime)?;
            let log = log.or_else(|| {
                profile
                    .parent()
                    .map(|directory| directory.join("Netzwerkprotokolle"))
            });
            let kind = trigger_kind.as_deref().unwrap_or(match at.as_str() {
                "logon" => "logon",
                "startup" => "startup",
                _ => "daily",
            });
            let trigger = Trigger::parse(kind, &at).map_err(|error| (2, error))?;
            if trigger.requires_background() && !background {
                return Err((
                    2,
                    invalid("Systemstart benötigt --background-password-stdin."),
                ));
            }
            let task = ScheduledScript {
                name: &name,
                trigger,
                executable: &executable,
                script: &script,
                profile: &profile,
                log: log.as_deref(),
            };
            if background {
                let stdin = io::stdin();
                if stdin.is_terminal() {
                    return Err((
                        2,
                        invalid("Leite das Windows-Kontopasswort über eine Pipe an stdin weiter."),
                    ));
                }
                let password = read_password(stdin.lock()).map_err(|error| (2, error))?;
                task.register_background(&password).map_err(runtime)?;
            } else {
                task.register().map_err(runtime)?;
            }
            let mut result = json!({"created": name, "trigger": kind, "schedule": at, "log_dir": log, "background": background});
            if matches!(trigger, Trigger::Daily(_)) {
                result["daily_at"] = json!(at);
            }
            println!("{result}");
        }
        Some("list") if args.next().is_none() => {
            for task in task_schedule::list().map_err(runtime)? {
                println!(
                    "{}",
                    json!({"name": task.name, "state": task.state, "last_result": task.last_result, "background": task.background})
                );
            }
        }
        Some(op @ ("run" | "remove")) => {
            let name = string_arg(args.next()).ok_or_else(usage)?;
            if args.next().is_some() {
                return Err(usage());
            }
            if op == "run" {
                task_schedule::run(&name)
            } else {
                task_schedule::remove(&name)
            }
            .map_err(runtime)?;
            println!("{}", json!({"action": op, "name": name}));
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn string_arg(value: Option<OsString>) -> Option<String> {
    value.and_then(|value| value.into_string().ok())
}

fn read_password(input: impl io::Read) -> io::Result<Zeroizing<String>> {
    const LIMIT: usize = 8192;
    let mut secret = Zeroizing::new(String::new());
    input.take(LIMIT as u64 + 1).read_to_string(&mut secret)?;
    if secret.len() > LIMIT {
        return Err(invalid("Das über stdin gelesene Passwort ist zu lang."));
    }
    if let Some(line) = secret
        .strip_suffix("\r\n")
        .or_else(|| secret.strip_suffix('\n'))
    {
        let length = line.len();
        secret.truncate(length);
    }
    if secret.is_empty() || secret.contains(['\0', '\r', '\n']) {
        return Err(invalid(
            "Erwartet wird genau ein Windows-Kontopasswort als UTF-8-Zeile.",
        ));
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_pipe_does_not_change_spaces_or_take_multiple_lines() {
        assert_eq!(
            read_password("  aß: # \r\n".as_bytes()).unwrap().as_str(),
            "  aß: # "
        );
        for data in ["\n", "one\ntwo", "one\0two"] {
            assert!(read_password(data.as_bytes()).is_err());
        }
    }
}
