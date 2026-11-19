#[path = "arco-net/keep_up_to_date.rs"]
mod keep_up_to_date;
#[path = "../network_profile.rs"]
mod network_profile;
#[cfg(windows)]
#[path = "arco-net/schedule_cli.rs"]
mod schedule_cli;
#[path = "arco-net/synchronize.rs"]
mod synchronize;
#[path = "arco-net/transfer_batch.rs"]
mod transfer_batch;
#[path = "arco-net/transfer_tree.rs"]
mod transfer_tree;

use arco_app::{
    network_crypto,
    network_sync::{FtpEndpoint, check_transfer_mask},
    network_transfer,
};
use arco_core::{
    files,
    profile::{FtpProfile, Profile},
};
use arco_windows::ftp::{self, Session, Url};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use zeroize::Zeroizing;

const SCRIPT_LIMIT: u64 = 1024 * 1024;

fn main() -> ExitCode {
    let mut raw = std::env::args_os().skip(1).peekable();
    #[cfg(windows)]
    if raw.peek().is_some_and(|arg| arg == "--ssh-console-bridge") {
        return finish(ftp::ssh_console_bridge().map_err(|error| (1, error)));
    }
    #[cfg(windows)]
    if raw.peek().is_some_and(|arg| arg == "schedule") {
        raw.next();
        return finish(schedule_cli::run(raw));
    }
    finish(arguments(raw).and_then(|args| {
        if let Some(args) = args {
            execute_script(&args).map_err(|error| (1, error))
        } else {
            println!("arco-net --script DATEI|- [--profile DATEI] [--password-env NAME] [--log-dir ORDNER]\n- liest ein UTF-8-Skript von stdin. JSON-Zeilen auf stdout. Exitcodes 0 Erfolg, 1 Skriptfehler, 2 Programmargumente.\narco-net schedule --help");
            Ok(())
        }
    }))
}

fn finish(result: Result<(), (u8, io::Error)>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, error)) => {
            eprintln!("{}", json!({"error": error.to_string(), "exit": code}));
            ExitCode::from(code)
        }
    }
}

struct Arguments {
    script: PathBuf,
    profile: PathBuf,
    password_env: Option<String>,
    log_dir: Option<PathBuf>,
}

fn arguments(
    mut args: impl Iterator<Item = OsString>,
) -> Result<Option<Arguments>, (u8, io::Error)> {
    let mut script = None;
    let mut profile = None;
    let mut password_env = None;
    let mut log_dir = None;
    while let Some(option) = args.next() {
        let invalid_option = || {
            (
                2,
                invalid("Unbekannte oder wiederholte Option. Verwende --help."),
            )
        };
        match option.to_str() {
            Some("--help" | "-h") => return Ok(None),
            Some("--script") if script.is_none() => script = args.next().map(PathBuf::from),
            Some("--profile") if profile.is_none() => {
                profile = Some(args.next().map(PathBuf::from).ok_or_else(invalid_option)?);
            }
            Some("--log-dir") if log_dir.is_none() => {
                log_dir = Some(args.next().map(PathBuf::from).ok_or_else(invalid_option)?);
            }
            Some("--password-env") if password_env.is_none() => {
                password_env = Some(
                    args.next()
                        .and_then(|name| name.into_string().ok())
                        .filter(|name| {
                            !name.is_empty()
                                && name
                                    .bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                        })
                        .ok_or_else(invalid_option)?,
                );
            }
            _ => return Err(invalid_option()),
        }
    }
    let script = script.ok_or_else(|| (2, invalid("Gib eine Skriptdatei mit --script an.")))?;
    let profile = profile
        .map_or_else(default_profile_path, Ok)
        .map_err(|error| (2, error))?;
    Ok(Some(Arguments {
        script,
        profile,
        password_env,
        log_dir,
    }))
}

fn default_profile_path() -> io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map_or_else(std::env::current_dir, Ok)?;
    Ok(base.join("Arco Commander").join("profile.json"))
}

fn execute_script(args: &Arguments) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let Some(directory) = &args.log_dir else {
        return run(args, &mut out);
    };
    std::fs::create_dir_all(directory)?;
    let (mut file, _) = tempfile::Builder::new()
        .prefix("arco-net-")
        .suffix(".jsonl")
        .tempfile_in(directory)?
        .keep()
        .map_err(|error| error.error)?;
    writeln!(
        file,
        "{}",
        json!({"status": "started", "utc": chrono::Utc::now().to_rfc3339()})
    )?;
    struct Output<'a, W> {
        out: &'a mut W,
        log: &'a mut File,
    }
    impl<W: Write> Write for Output<'_, W> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.log.write_all(data)?;
            match self.out.write_all(data) {
                Ok(()) => Ok(data.len()),
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(data.len()),
                Err(error) => Err(error),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            self.log.flush()?;
            self.out.flush()
        }
    }
    let result = run(
        args,
        &mut Output {
            out: &mut out,
            log: &mut file,
        },
    );
    let final_event = match &result {
        Ok(()) => json!({"status": "completed", "utc": chrono::Utc::now().to_rfc3339(), "exit": 0}),
        Err(error) => {
            json!({"status": "failed", "utc": chrono::Utc::now().to_rfc3339(), "exit": 1, "error": error.to_string()})
        }
    };
    writeln!(file, "{final_event}")?;
    file.flush()?;
    result
}

enum Command<'a> {
    Watch(keep_up_to_date::KeepUpToDate<'a>),
    Open(&'a str),
    Close(Option<usize>),
    Sessions,
    UseSession(usize),
    Exit,
    Pwd,
    LocalPwd,
    LocalList(Option<&'a str>),
    Echo(Vec<&'a str>),
    FailOnNoMatch(bool),
    List(Option<&'a str>),
    Stat(&'a str),
    Chmod(&'a str, u16),
    Checksum(&'a str),
    Cd(&'a str),
    Lcd(&'a str),
    Put(Transfer<'a>),
    Get(Transfer<'a>),
    GetUrl(Transfer<'a>),
    PutEncrypted(&'a str, &'a str),
    GetEncrypted(&'a str, &'a str, Option<&'a str>),
    Mkdir(&'a str),
    Move(&'a str, &'a str),
    Remove(&'a str, bool, bool),
    TrashList,
    TrashPut(&'a str),
    TrashRestore(&'a str, Option<&'a str>),
    Synchronize(synchronize::Synchronize<'a>),
}

fn parse(words: &[String]) -> io::Result<Option<Command<'_>>> {
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    Ok(Some(match words.as_slice() {
        ["keepuptodate", args @ ..] => Command::Watch(keep_up_to_date::KeepUpToDate::parse(args)?),
        [] => return Ok(None),
        ["open", profile] => Command::Open(profile),
        ["close"] => Command::Close(None),
        ["close", number] => Command::Close(Some(session_number(number)?)),
        ["session"] => Command::Sessions,
        ["session", number] => Command::UseSession(session_number(number)?),
        ["exit"] => Command::Exit,
        ["pwd"] => Command::Pwd,
        ["lpwd"] => Command::LocalPwd,
        ["lls"] => Command::LocalList(None),
        ["lls", pattern] => Command::LocalList(Some(pattern)),
        ["echo", message @ ..] => Command::Echo(message.to_vec()),
        ["option", "failonnomatch", state @ ("on" | "off")] => {
            Command::FailOnNoMatch(*state == "on")
        }
        ["ls" | "dir"] => Command::List(None),
        ["ls" | "dir", path] => Command::List(Some(path)),
        ["stat", name] => Command::Stat(name),
        ["chmod", mode, name] => {
            let mode = u16::from_str_radix(mode, 8)
                .ok()
                .filter(|mode| *mode <= 0o7777)
                .ok_or_else(|| {
                    invalid("Dateirechte als Oktalzahl zwischen 0000 und 7777 angeben.")
                })?;
            Command::Chmod(name, mode)
        }
        ["checksum", name] => Command::Checksum(name),
        ["cd", path] => Command::Cd(path),
        ["lcd", path] => Command::Lcd(path),
        ["put" | "send" | "mput", args @ ..] => Command::Put(Transfer::parse(args, true)?),
        ["get" | "recv" | "mget", args @ ..] => Command::Get(Transfer::parse(args, true)?),
        ["get-url", args @ ..] => Command::GetUrl(Transfer::parse(args, false)?),
        ["put-encrypted", source, password_env] => Command::PutEncrypted(source, password_env),
        ["get-encrypted", source, password_env] => {
            Command::GetEncrypted(source, password_env, None)
        }
        ["get-encrypted", source, password_env, folder] => {
            Command::GetEncrypted(source, password_env, Some(folder))
        }
        ["mkdir", name] => Command::Mkdir(name),
        ["mv", from, to] => Command::Move(from, to),
        ["rm", name] => Command::Remove(name, false, false),
        ["rmdir", name] => Command::Remove(name, true, false),
        ["rm", "-permanent", name] => Command::Remove(name, false, true),
        ["rmdir", "-permanent", name] => Command::Remove(name, true, true),
        ["trash"] => Command::TrashList,
        ["trash", "put", name] => Command::TrashPut(name),
        ["trash", "restore", slot] => Command::TrashRestore(slot, None),
        ["trash", "restore", slot, destination] => Command::TrashRestore(slot, Some(destination)),
        ["synchronize", direction, paths @ ..] => {
            Command::Synchronize(synchronize::Synchronize::parse(direction, paths)?)
        }
        _ => {
            return Err(invalid(
                "Unbekannter Skriptbefehl oder falsche Anzahl Argumente.",
            ));
        }
    }))
}

fn session_number(value: &str) -> io::Result<usize> {
    value
        .parse()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| invalid("Gib eine positive Sitzungsnummer an."))
}

fn encryption_passphrase(variable: &str) -> io::Result<Zeroizing<String>> {
    if variable.is_empty()
        || !variable
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(invalid(
            "Gib den Namen einer Umgebungsvariablen für die Passphrase an.",
        ));
    }
    std::env::var(variable).map(Zeroizing::new).map_err(|_| {
        invalid("Die Umgebungsvariable mit der Passphrase fehlt oder enthält kein Unicode.")
    })
}

struct Transfer<'a> {
    source: &'a str,
    more: Vec<&'a str>,
    target: Option<&'a str>,
    preset: Option<&'a str>,
    speed: Option<u32>,
    preserve_time: Option<bool>,
    mode: Option<ftp::PreferredTransferMode>,
    masks: &'a str,
    latest: bool,
    fail_on_no_match: bool,
    resume: bool,
    recursive: bool,
}

impl<'a> Transfer<'a> {
    fn parse(args: &[&'a str], allow_batch: bool) -> io::Result<Self> {
        let mut paths = Vec::new();
        let mut positional = false;
        let mut transfer = Self {
            source: "",
            more: Vec::new(),
            target: None,
            preset: None,
            speed: None,
            preserve_time: None,
            mode: None,
            masks: "",
            latest: false,
            fail_on_no_match: false,
            resume: false,
            recursive: false,
        };
        for &arg in args {
            if !positional {
                if arg == "--" {
                    positional = true;
                    continue;
                }
                if let Some(name) = arg.strip_prefix("-preset=") {
                    if name.is_empty() || transfer.preset.replace(name).is_some() {
                        return Err(invalid("Gib mit -preset genau eine Transfervorgabe an."));
                    }
                    continue;
                }
                if let Some(rate) = arg.strip_prefix("-speed=") {
                    transfer.speed = Some(
                        rate.parse()
                            .ok()
                            .filter(|rate| *rate <= 1_048_576)
                            .ok_or_else(|| invalid("-speed erwartet 0 bis 1048576 KiB/s."))?,
                    );
                    continue;
                }
                if let Some(mode) = arg.strip_prefix("-transfer=") {
                    transfer.mode = Some(match mode {
                        "binary" => ftp::PreferredTransferMode::Binary,
                        "ascii" => ftp::PreferredTransferMode::Text,
                        "automatic" => ftp::PreferredTransferMode::Automatic,
                        _ => {
                            return Err(invalid(
                                "-transfer erwartet binary, ascii oder automatic.",
                            ));
                        }
                    });
                    continue;
                }
                if let Some(masks) = arg.strip_prefix("-filemask=") {
                    if masks.len() > 4096 {
                        return Err(invalid("Die Transfermaske ist zu lang."));
                    }
                    arco_core::sync::FileMasks::new(masks)?;
                    transfer.masks = masks;
                    continue;
                }
                if matches!(arg, "-preservetime" | "-nopreservetime") {
                    transfer.preserve_time = Some(arg == "-preservetime");
                    continue;
                }
                if allow_batch && arg == "-latest" {
                    transfer.latest = true;
                    continue;
                }
                if allow_batch && arg == "-failonnomatch" {
                    transfer.fail_on_no_match = true;
                    continue;
                }
                if arg == "-resume" {
                    transfer.resume = true;
                    continue;
                }
                if allow_batch && arg == "-recursive" {
                    transfer.recursive = true;
                    continue;
                }
            }
            paths.push(arg);
        }
        match paths.as_slice() {
            [source] => Ok(Self { source, ..transfer }),
            [source, target] => Ok(Self {
                source,
                target: Some(target),
                ..transfer
            }),
            [source, more @ .., target] if allow_batch => Ok(Self {
                source,
                more: more.to_vec(),
                target: Some(target),
                ..transfer
            }),
            _ => Err(invalid(
                "Gib eine Quelldatei und optional einen Zielpfad an.",
            )),
        }
    }

    fn session(
        &self,
        profile: &Profile,
        session: &Arc<Session>,
        directory: &Url,
        local: &Path,
        name: &str,
    ) -> io::Result<Arc<Session>> {
        let session = self.effective_session(profile, session, directory, local.parent())?;
        check_transfer_mask(&session, name)?;
        if !arco_core::sync::FileMasks::new(self.masks)?.includes(Path::new(name)) {
            return Err(invalid("Die Datei ist durch -filemask ausgeschlossen."));
        }
        Ok(session)
    }

    fn effective_session(
        &self,
        profile: &Profile,
        session: &Arc<Session>,
        directory: &Url,
        local: Option<&Path>,
    ) -> io::Result<Arc<Session>> {
        let session = arco_app::network_transfer::resolve(
            &profile.network_transfer_presets,
            session,
            directory,
            local,
            self.preset,
        )?;
        if self.resume && !(session.is_sftp() || session.security().is_ftp()) {
            return Err(invalid("-resume benötigt FTP, FTPS oder SFTP."));
        }
        if self.resume
            && self
                .mode
                .is_some_and(|mode| mode != ftp::PreferredTransferMode::Binary)
        {
            return Err(invalid("-resume benötigt eine binäre Übertragung."));
        }
        if self.mode.is_none()
            && self.speed.is_none()
            && self.preserve_time.is_none()
            && !self.resume
        {
            return Ok(session);
        }
        let mut settings = session.transfer_settings().clone();
        if let Some(mode) = self.mode {
            if mode != ftp::PreferredTransferMode::Binary && !session.security().is_ftp() {
                return Err(invalid(
                    "ASCII und automatische Textauswahl benötigen FTP oder FTPS.",
                ));
            }
            settings.mode = mode;
        }
        if let Some(rate) = self.speed {
            settings.speed_kib = (rate > 0).then_some(rate);
        }
        if self.preserve_time.is_some() {
            settings.preserve_time = self.preserve_time;
        }
        if self.resume {
            settings.mode = ftp::PreferredTransferMode::Binary;
        }
        Ok(Arc::new(session.with_transfer_snapshot(settings)))
    }

    fn download_file(
        &self,
        session: &Session,
        directory: &Url,
        name: &str,
        target: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.resume {
            session.download_resuming(directory, name, target, cancel)
        } else {
            session.download_preferred(directory, name, target, cancel, |_| {})
        }
    }

    fn upload_file(
        &self,
        session: &Session,
        directory: &Url,
        source: &Path,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.resume {
            if session.is_sftp() {
                return session.upload_sftp_resuming(directory, source, name, cancel, |_| {});
            }
            let listing = session.list_sync(directory, cancel)?;
            if listing.entries.iter().any(|item| item.name == name) {
                listing.regular_file(name)?;
                return session.upload_resuming(directory, source, name, cancel);
            }
        }
        session.upload_with_mode(
            directory,
            source,
            name,
            false,
            session.preferred_transfer_mode(name),
            cancel,
        )
    }
}

fn words(line: &str) -> io::Result<Vec<String>> {
    let mut output = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                word.push('"');
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '#' if !quoted && !started => break,
            c if !quoted && c.is_whitespace() => {
                if started {
                    output.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err(invalid("Ein Anführungszeichen im Skript ist offen."));
    }
    if started {
        output.push(word);
    }
    Ok(output)
}

fn run(args: &Arguments, output: &mut impl Write) -> io::Result<()> {
    let mut text = String::new();
    let source: Box<dyn Read> = if args.script == Path::new("-") {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(&args.script)?)
    };
    source.take(SCRIPT_LIMIT + 1).read_to_string(&mut text)?;
    if text.len() as u64 > SCRIPT_LIMIT {
        return Err(invalid("Das Skript überschreitet 1 MiB."));
    }
    let lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .enumerate()
        .map(|(index, line)| words(line).map_err(|error| at_line(index + 1, error)))
        .collect::<io::Result<Vec<_>>>()?;
    let commands = lines
        .iter()
        .enumerate()
        .map(|(index, words)| parse(words).map_err(|error| at_line(index + 1, error)))
        .collect::<io::Result<Vec<_>>>()?;
    if args.password_env.is_some()
        && commands
            .iter()
            .filter(|command| matches!(command, Some(Command::Open(_) | Command::GetUrl(..))))
            .count()
            != 1
    {
        return Err(invalid(
            "--password-env ist für Skripte mit genau einem open- oder get-url-Befehl vorgesehen.",
        ));
    }
    let profile = Profile::load(&args.profile)?;
    let mut local = std::env::current_dir()?;
    let mut remote = RemoteSessions::default();
    let cancel = AtomicBool::new(false);
    for (index, command) in commands.into_iter().enumerate() {
        let Some(command) = command else { continue };
        if matches!(command, Command::Exit) {
            break;
        }
        let mut publish = |event| -> io::Result<()> {
            serde_json::to_writer(&mut *output, &json!({"line": index + 1, "event": event}))?;
            writeln!(output)?;
            output.flush()
        };
        let (event, interrupted) = match command {
            Command::Watch(watch) => {
                let current = remote
                    .active
                    .and_then(|id| remote.entries.get(&id))
                    .ok_or_else(|| {
                        at_line(index + 1, invalid("Öffne zuerst ein Netzwerkprofil."))
                    })?;
                watch.execute(&profile, current, &local, &mut publish)
            }
            command => execute(
                command,
                &profile,
                &mut remote,
                &mut local,
                args.password_env.as_deref(),
                &cancel,
            )
            .map(|event| (event, false)),
        }
        .map_err(|error| at_line(index + 1, error))?;
        publish(event)?;
        if interrupted {
            break;
        }
    }
    Ok(())
}

struct OpenSession {
    endpoint: FtpEndpoint,
    name: String,
    profile: Option<String>,
}

#[derive(Default)]
struct RemoteSessions {
    entries: BTreeMap<usize, OpenSession>,
    active: Option<usize>,
    last_id: usize,
    fail_on_no_match: bool,
}

fn execute(
    command: Command<'_>,
    profile: &Profile,
    remote: &mut RemoteSessions,
    local: &mut PathBuf,
    password_env: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<Value> {
    match command {
        Command::Open(name) => {
            let (label, session, file, profile_name) =
                if let Some(target) = profile.ftp_profiles.get(name) {
                    (
                        name.to_owned(),
                        connect(profile, name, target, password_env)?,
                        None,
                        Some(name.to_owned()),
                    )
                } else if name.contains("://") {
                    let link = arco_core::session_url::resolve(name, &profile.ftp_profiles)?;
                    let profile_name = link.profile.as_deref().unwrap_or_default();
                    let label = link
                        .profile
                        .clone()
                        .unwrap_or_else(|| link.connection.address.clone());
                    (
                        label,
                        connect(profile, profile_name, &link.connection, password_env)?,
                        link.file,
                        link.profile,
                    )
                } else {
                    return Err(invalid("Das Netzwerkprofil ist nicht vorhanden."));
                };
            let directory = session.initial_directory();
            let listing = session.list(&directory, cancel)?;
            if let Some(name) = &file {
                listing.regular_file(name)?;
            }
            remote.last_id += 1;
            let id = remote.last_id;
            remote.entries.insert(
                id,
                OpenSession {
                    endpoint: (Arc::new(session), directory),
                    name: label.clone(),
                    profile: profile_name,
                },
            );
            remote.active = Some(id);
            return Ok(match file {
                Some(file) => json!({"open": label, "file": file, "session": id}),
                None => json!({"open": label, "session": id}),
            });
        }
        Command::GetUrl(transfer) => {
            let link = arco_core::session_url::resolve(transfer.source, &profile.ftp_profiles)?;
            let name = link
                .file
                .ok_or_else(|| invalid("get-url benötigt die URL einer Serverdatei."))?;
            let target = match transfer.target {
                Some(path) => local.join(path),
                None => local.join(files::leaf_name(OsStr::new(&name))?),
            };
            let session = Arc::new(connect(
                profile,
                link.profile.as_deref().unwrap_or_default(),
                &link.connection,
                password_env,
            )?);
            let directory = session.initial_directory();
            let session = transfer.session(profile, &session, &directory, &target, &name)?;
            session.list_sync(&directory, cancel)?.regular_file(&name)?;
            transfer.download_file(&session, &directory, &name, &target, cancel)?;
            return Ok(json!({"downloaded": target}));
        }
        Command::Close(requested) => {
            if let Some(id) = requested.or(remote.active) {
                remote
                    .entries
                    .remove(&id)
                    .ok_or_else(|| invalid("Die Sitzung ist nicht geöffnet."))?;
                if remote.active == Some(id) {
                    remote.active = remote.entries.last_key_value().map(|(id, _)| *id);
                }
            }
            return Ok(json!({"closed": true}));
        }
        Command::Sessions => {
            let sessions = remote
                .entries
                .iter()
                .map(|(id, session)| {
                    json!({
                        "id": id,
                        "name": session.name,
                        "active": remote.active == Some(*id),
                        "remote": Session::directory_path(&session.endpoint.1),
                    })
                })
                .collect::<Vec<_>>();
            return Ok(json!({"sessions": sessions}));
        }
        Command::UseSession(id) => {
            let session = remote
                .entries
                .get(&id)
                .ok_or_else(|| invalid("Die Sitzung ist nicht geöffnet."))?;
            remote.active = Some(id);
            return Ok(
                json!({"session": id, "remote": Session::directory_path(&session.endpoint.1)}),
            );
        }
        Command::Lcd(path) => {
            let next = local.join(path).canonicalize()?;
            if !next.is_dir() {
                return Err(invalid("Der lokale Pfad ist kein Ordner."));
            }
            *local = next;
            return Ok(json!({"local": local}));
        }
        Command::LocalPwd => return Ok(json!({"local": local})),
        Command::LocalList(pattern) => return list_local(local, pattern),
        Command::Echo(message) => return Ok(json!({"message": message.join(" ")})),
        Command::FailOnNoMatch(enabled) => {
            remote.fail_on_no_match = enabled;
            return Ok(json!({"option": "failonnomatch", "enabled": enabled}));
        }
        _ => (),
    }
    let fail_on_no_match = remote.fail_on_no_match;
    let remote = remote
        .active
        .and_then(|id| remote.entries.get_mut(&id))
        .ok_or_else(|| invalid("Öffne zuerst ein Netzwerkprofil."))?;
    if let Command::Synchronize(settings) = command {
        return settings.execute(
            local,
            &remote.endpoint,
            profile,
            remote.profile.as_deref(),
            cancel,
        );
    }
    let (session, directory) = &mut remote.endpoint;
    match command {
        Command::Pwd => Ok(json!({"remote": Session::directory_path(directory)})),
        Command::List(path) => list_remote(session, directory, path, fail_on_no_match, cancel),
        Command::Stat(name) => {
            Ok(json!({"entry": entry_json(&entry(session, directory, name, cancel)?)}))
        }
        Command::Chmod(name, permissions) => {
            let entry = entry(session, directory, name, cancel)?;
            session.set_unix_permissions(directory, &entry, permissions, cancel)?;
            Ok(json!({"permissions": format!("{permissions:04o}"), "name": name}))
        }
        Command::Checksum(name) => {
            let entry = entry(session, directory, name, cancel)?;
            if session.is_sftp() {
                let digest = session.sftp_sha256(directory, &entry, cancel, |_| {})?;
                return Ok(json!({"checksum": digest, "algorithm": "SHA-256", "name": name}));
            }
            let supported = || {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Der Server stellt keine geeignete Prüfsumme bereit.",
                )
            };
            let method = session
                .sync_checksum_method(directory, cancel)?
                .ok_or_else(supported)?;
            let (algorithm, digest) = session
                .sync_checksum(directory, &entry, method, cancel)?
                .ok_or_else(supported)?;
            Ok(json!({"checksum": digest, "algorithm": algorithm, "name": name}))
        }
        Command::Cd(path) => {
            let next = if path == ".." {
                session.parent_directory(directory)?
            } else {
                session.relative_directory(directory, Path::new(path))?
            };
            session.list(&next, cancel)?;
            *directory = next;
            Ok(json!({"remote": Session::directory_path(directory)}))
        }
        Command::Mkdir(name) => {
            session.create_sync_directory(directory, name, cancel)?;
            Ok(json!({"created": name}))
        }
        Command::Move(from, to) => {
            let entry = entry(session, directory, from, cancel)?;
            session.rename_sync_entry(directory, &entry, to, cancel)?;
            Ok(json!({"renamed": from, "name": to}))
        }
        Command::Remove(name, directory_only, permanent) => {
            let entry = entry(session, directory, name, cancel)?;
            if entry.directory != Some(directory_only) {
                return Err(invalid("Der Eintrag hat einen anderen Dateityp."));
            }
            if session.protects_recycle_directory(directory, &entry, cancel)? {
                return Err(invalid(
                    "Öffne den Server-Papierkorb, um seine Inhalte zu entfernen.",
                ));
            }
            if !permanent && session.will_recycle(directory) {
                let slot = session.recycle_entry(directory, &entry, cancel)?;
                return Ok(json!({"recycled": name, "slot": slot}));
            }
            if directory_only {
                let folder = session.relative_directory(directory, Path::new(name))?;
                if !session.list_sync(&folder, cancel)?.entries.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::DirectoryNotEmpty,
                        "Der Serverordner ist nicht leer.",
                    ));
                }
            }
            session.delete_sync_entry(directory, &entry, cancel)?;
            Ok(json!({"removed": name}))
        }
        Command::TrashList => {
            let bin = session.ensure_recycle_directory(cancel)?;
            let entries = session.list_sync(&bin, cancel)?;
            let slots: Vec<_> = entries
                .entries
                .iter()
                .filter(|entry| entry.directory == Some(true))
                .map(entry_json)
                .collect();
            Ok(json!({"trash": Session::directory_path(&bin), "slots": slots}))
        }
        Command::TrashPut(source) => {
            let (parent, name) =
                session.download_location(directory, source.trim_end_matches('/'))?;
            let entry = entry(session, &parent, &name, cancel)?;
            let slot = session.recycle_entry(&parent, &entry, cancel)?;
            Ok(json!({"recycled": name, "slot": slot}))
        }
        Command::TrashRestore(slot, destination) => {
            let target = session.recycle_restore_destination(destination.unwrap_or("."))?;
            let entry = session.restore_recycled_entry(slot, &target, cancel)?;
            Ok(json!({"restored": entry.name, "directory": Session::directory_path(&target)}))
        }
        Command::PutEncrypted(path, password_env) => {
            let passphrase = encryption_passphrase(password_env)?;
            let source = local.join(path);
            let endpoint = network_transfer::endpoint(
                &profile.network_transfer_presets,
                &(session.clone(), directory.clone()),
                source.parent(),
                None,
            )?;
            let entry = network_crypto::upload(&endpoint, &source, &passphrase, cancel, |_| {})?;
            Ok(json!({"uploaded": entry.name, "encrypted": true}))
        }
        Command::GetEncrypted(path, password_env, folder) => {
            let passphrase = encryption_passphrase(password_env)?;
            let (parent, name) = session.download_location(directory, path)?;
            let folder = local.join(folder.unwrap_or("."));
            let endpoint = network_transfer::endpoint(
                &profile.network_transfer_presets,
                &(session.clone(), parent),
                Some(&folder),
                None,
            )?;
            let entry = entry(&endpoint.0, &endpoint.1, &name, cancel)?;
            let result =
                network_crypto::download(&endpoint, &entry, &folder, &passphrase, cancel, |_| {})?;
            Ok(json!({"downloaded": result, "encrypted": true}))
        }
        Command::Put(mut transfer) => {
            transfer.fail_on_no_match |= fail_on_no_match;
            if transfer.recursive || transfer.sources().any(|source| local.join(source).is_dir()) {
                return transfer.upload_tree(
                    profile,
                    session,
                    directory,
                    local,
                    remote.profile.as_deref(),
                    cancel,
                );
            }
            if transfer.is_batch(true, local) {
                return transfer.upload_batch(profile, session, directory, local, cancel);
            }
            let source = local.join(transfer.source);
            let name = transfer
                .target
                .or_else(|| source.file_name().and_then(OsStr::to_str))
                .ok_or_else(|| invalid("Für den Upload fehlt ein gültiger Name."))?;
            files::regular_metadata(&source)?;
            let session = transfer.session(profile, session, directory, &source, name)?;
            transfer.upload_file(&session, directory, &source, name, cancel)?;
            Ok(json!({"uploaded": name}))
        }
        Command::Get(mut transfer) => {
            transfer.fail_on_no_match |= fail_on_no_match;
            if transfer.recursive || transfer.sources().any(|source| source.ends_with('/')) {
                return transfer.download_tree(
                    profile,
                    session,
                    directory,
                    local,
                    remote.profile.as_deref(),
                    cancel,
                );
            }
            if transfer.is_batch(false, local) {
                return transfer.download_batch(
                    profile,
                    session,
                    directory,
                    local,
                    remote.profile.as_deref(),
                    cancel,
                );
            }
            let (parent, name) = session.download_location(directory, transfer.source)?;
            if session.list(&parent, cancel).is_ok_and(|listing| {
                listing
                    .entries
                    .iter()
                    .any(|entry| entry.name == name && entry.directory == Some(true))
            }) {
                return transfer.download_tree(
                    profile,
                    session,
                    directory,
                    local,
                    remote.profile.as_deref(),
                    cancel,
                );
            }
            files::leaf_name(OsStr::new(&name))?;
            let target = local.join(transfer.target.unwrap_or(&name));
            let session = transfer.session(profile, session, &parent, &target, &name)?;
            transfer.download_file(&session, &parent, &name, &target, cancel)?;
            Ok(json!({"downloaded": target}))
        }
        _ => unreachable!(),
    }
}

fn list_remote(
    session: &Session,
    current: &Url,
    requested: Option<&str>,
    fail_on_no_match: bool,
    cancel: &AtomicBool,
) -> io::Result<Value> {
    let requested = requested.unwrap_or("");
    let requested = requested.strip_prefix("./").unwrap_or(requested);
    let leaf = requested.rsplit('/').next().unwrap_or_default();
    let (directory, mask) = if leaf.contains(['*', '?']) {
        let (directory, mask) = session.download_location(current, requested)?;
        (directory, Some(files::compile_masks(&mask)?))
    } else {
        let directory = match requested {
            "." | "" => current.clone(),
            ".." => session.parent_directory(current)?,
            url if url.contains("://") => session.directory_url(url)?,
            absolute if absolute.starts_with('/') => session.directory(absolute)?,
            relative => session.relative_directory(current, Path::new(relative))?,
        };
        (directory, None)
    };
    let listing = session.list(&directory, cancel)?;
    let entries = listing
        .entries
        .iter()
        .filter(|entry| mask.as_ref().is_none_or(|mask| mask.is_match(&entry.name)))
        .map(entry_json)
        .collect::<Vec<_>>();
    if entries.is_empty() && mask.is_some() && fail_on_no_match {
        return Err(invalid("Die Server-Dateimaske hat keine Treffer."));
    }
    Ok(json!({"entries": entries}))
}

fn list_local(current: &Path, pattern: Option<&str>) -> io::Result<Value> {
    let requested = pattern.map_or_else(|| current.to_path_buf(), |path| current.join(path));
    let (directory, mask) = if let Some(mask) = requested
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| name.contains(['*', '?']))
    {
        (
            requested.parent().unwrap_or(current),
            Some(files::compile_masks(mask)?),
        )
    } else {
        (requested.as_path(), None)
    };
    let mut entries = Vec::new();
    for entry in directory.read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        if mask.as_ref().is_some_and(|mask| !mask.is_match(&name)) {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified = metadata
            .modified()
            .ok()
            .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339());
        entries.push((name, metadata.is_dir(), metadata.len(), modified));
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let entries = entries
        .into_iter()
        .map(|(name, directory, size, modified)| {
            json!({
                "name": name.to_string_lossy(),
                "directory": directory,
                "size": (!directory).then_some(size),
                "modified_utc": modified,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"local": directory, "entries": entries}))
}

fn entry_json(entry: &ftp::Entry) -> Value {
    json!({"name": entry.name, "directory": entry.directory, "size": entry.size, "modified_utc": entry.modified})
}

fn entry(
    session: &Session,
    directory: &Url,
    name: &str,
    cancel: &AtomicBool,
) -> io::Result<ftp::Entry> {
    files::leaf_name(OsStr::new(name))?;
    session
        .list_sync(directory, cancel)?
        .entries
        .into_iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Der Servereintrag wurde nicht gefunden.",
            )
        })
}

fn connect(
    profiles: &Profile,
    name: &str,
    profile: &FtpProfile,
    password_env: Option<&str>,
) -> io::Result<Session> {
    let connection = network_profile::connection(profile)?;
    let secret = credential(profile, &connection, password_env)?;
    let mut hops = Vec::new();
    for (_, profile) in network_profile::jump_profiles(name, profile, &profiles.ftp_profiles)? {
        let hop = network_profile::connection(profile)?;
        let secret = credential(profile, &hop, None)?;
        hops.push((hop, secret));
    }
    let connection = if hops.is_empty() {
        connection
    } else {
        connection.with_ssh_jumps(hops)?
    };
    connection.authenticate(&secret)
}

fn credential(
    profile: &FtpProfile,
    connection: &ftp::Connection,
    env: Option<&str>,
) -> io::Result<Zeroizing<String>> {
    if let Some(env) = env {
        return std::env::var(env).map(Zeroizing::new).map_err(|_| {
            invalid("Die angegebene Passwort-Umgebungsvariable ist nicht verfügbar.")
        });
    }
    if let Some(secret) = network_profile::saved_secret(profile, connection)? {
        return Ok(secret);
    }
    if profile.user.is_empty()
        || profile.ssh_private_key.is_some()
        || profile.ssh_agent.is_some()
        || profile.ssh_kerberos.is_some()
    {
        return Ok(Zeroizing::default());
    }
    Err(invalid(
        "Das Netzwerkprofil benötigt ein gespeichertes Passwort oder --password-env.",
    ))
}

fn at_line(line: usize, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("Zeile {line}. {error}"))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_paths_preserve_backslashes_spaces_and_comments() {
        let input = words(r#"put "C:\Test Ordner\Grüße.txt" "Datei #1.txt" # Kommentar"#).unwrap();
        assert!(matches!(
            parse(&input).unwrap(),
            Some(Command::Put(Transfer {
                source: r"C:\Test Ordner\Grüße.txt",
                target: Some("Datei #1.txt"),
                preset: None,
                ..
            }))
        ));
        assert!(words("open \"unclear").is_err());
        assert!(parse(&words("call program").unwrap()).is_err());
    }

    #[test]
    fn transfer_preset_switch_preserves_quoted_names_and_literal_paths() {
        let input = words(r#"get "-preset=Archiv Kopie" -- "-preset=literal" "test.txt""#).unwrap();
        assert!(matches!(
            parse(&input).unwrap(),
            Some(Command::Get(Transfer {
                source: "-preset=literal",
                target: Some("test.txt"),
                preset: Some("Archiv Kopie"),
                ..
            }))
        ));
        assert!(parse(&words("put a -preset=x -preset=y").unwrap()).is_err());
    }
}
