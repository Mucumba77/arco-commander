//! Windows Task Scheduler integration for Arco network scripts.

use std::io;
use std::path::Path;
use windows::Win32::System::{
    Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    },
    TaskScheduler::{
        IDailyTrigger, IExecAction, ILogonTrigger, IMonthlyTrigger, IRegisteredTask,
        ITaskDefinition, ITaskFolder, ITaskService, IWeeklyTrigger, TASK_ACTION_EXEC, TASK_CREATE,
        TASK_INSTANCES_IGNORE_NEW, TASK_LOGON_INTERACTIVE_TOKEN, TASK_LOGON_PASSWORD,
        TASK_LOGON_TYPE, TASK_RUNLEVEL_LUA, TASK_STATE_DISABLED, TASK_STATE_QUEUED,
        TASK_STATE_READY, TASK_STATE_RUNNING, TASK_TRIGGER_BOOT, TASK_TRIGGER_DAILY,
        TASK_TRIGGER_LOGON, TASK_TRIGGER_MONTHLY, TASK_TRIGGER_TIME, TASK_TRIGGER_WEEKLY,
        TaskScheduler,
    },
    Variant::VARIANT,
};
use windows::core::{BSTR, Interface, Result as ComResult};
use windows_sys::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
use zeroize::{Zeroize, Zeroizing};

const PREFIX: &str = "Arco Commander - ";
const SOURCE: &str = "Arco Commander network script";

pub struct Task {
    pub name: String,
    pub state: &'static str,
    pub last_result: i32,
    pub background: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Daily(chrono::NaiveTime),
    Once(chrono::NaiveDateTime),
    Weekly(chrono::NaiveTime, i16),
    Monthly(chrono::NaiveTime, i32),
    Logon,
    Startup,
}

impl Trigger {
    pub fn requires_background(self) -> bool {
        self == Self::Startup
    }

    pub fn parse(kind: &str, value: &str) -> io::Result<Self> {
        let time = |at: &str| {
            chrono::NaiveTime::parse_from_str(at, "%H:%M")
                .ok()
                .filter(|_| at.len() == 5)
                .ok_or_else(|| invalid("Gib die lokale Uhrzeit im Format HH:MM ein."))
        };
        let calendar = || {
            let (at, days) = value
                .trim()
                .split_once(char::is_whitespace)
                .ok_or_else(|| invalid("Gib nach der Uhrzeit die gewünschten Tage ein."))?;
            Ok::<_, io::Error>((time(at)?, days.split(',').map(str::trim)))
        };
        match kind {
            "daily" => Ok(Self::Daily(time(value.trim())?)),
            "once" => chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%dT%H:%M")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M"))
                .map(Self::Once)
                .map_err(|_| invalid("Gib Datum und Uhrzeit im Format JJJJ-MM-TT HH:MM ein.")),
            "weekly" => {
                let (at, days) = calendar()?;
                let mut mask = 0;
                for day in days {
                    mask |= match day.to_ascii_lowercase().as_str() {
                        "sun" | "so" => 1,
                        "mon" | "mo" => 2,
                        "tue" | "di" => 4,
                        "wed" | "mi" => 8,
                        "thu" | "do" => 16,
                        "fri" | "fr" => 32,
                        "sat" | "sa" => 64,
                        _ => return Err(invalid("Gib Wochentage als Mo,Di,Mi,Do,Fr,Sa,So ein.")),
                    };
                }
                Ok(Self::Weekly(at, mask))
            }
            "monthly" => {
                let (at, days) = calendar()?;
                let mut mask = 0;
                for day in days {
                    let day = day
                        .parse::<u32>()
                        .ok()
                        .filter(|day| (1..=31).contains(day))
                        .ok_or_else(|| invalid("Gib Monatstage zwischen 1 und 31 ein."))?;
                    mask |= 1 << (day - 1);
                }
                Ok(Self::Monthly(at, mask))
            }
            "logon" if matches!(value.trim(), "" | "logon") => Ok(Self::Logon),
            "startup" if matches!(value.trim(), "" | "startup") => Ok(Self::Startup),
            _ => Err(invalid("Ungültiger Aufgabenauslöser oder Zeitwert.")),
        }
    }
}

pub struct ScheduledScript<'a> {
    pub name: &'a str,
    pub trigger: Trigger,
    pub executable: &'a Path,
    pub script: &'a Path,
    pub profile: &'a Path,
    pub log: Option<&'a Path>,
}

impl ScheduledScript<'_> {
    pub fn register(&self) -> io::Result<()> {
        self.register_with_password(None)
    }

    pub fn register_background(&self, password: &str) -> io::Result<()> {
        if password.is_empty() || password.contains('\0') {
            return Err(invalid("Gib das Passwort deines Windows-Kontos ein."));
        }
        self.register_with_password(Some(password))
    }

    fn register_with_password(&self, password: Option<&str>) -> io::Result<()> {
        if self.trigger.requires_background() && password.is_none() {
            return Err(invalid(
                "Systemstart benötigt die Ausführung ohne Windows-Anmeldung.",
            ));
        }
        let name = full_name(self.name)?;
        let launch = self.launch()?;
        let account = (password.is_some() || self.trigger == Trigger::Logon)
            .then(current_account)
            .transpose()?;
        let mode = if password.is_some() {
            TASK_LOGON_PASSWORD
        } else {
            TASK_LOGON_INTERACTIVE_TOKEN
        };
        let secret = password.map(TaskPassword::new);
        with_scheduler(|service, folder| {
            let definition = definition(service, &launch, mode, account.as_ref(), self.trigger)?;
            let empty = VARIANT::default();
            let user = account.map(VARIANT::from).unwrap_or_default();
            // TASK_CREATE rejects existing names. An explicit user action is required to delete a task.
            unsafe {
                folder.RegisterTaskDefinition(
                    &name,
                    &definition,
                    TASK_CREATE.0,
                    &user,
                    secret.as_ref().map_or(&empty, |secret| &secret.variant),
                    mode,
                    &empty,
                )?;
            }
            Ok(())
        })
    }

    fn launch(&self) -> io::Result<Launch> {
        if matches!(self.trigger, Trigger::Once(at) if at <= chrono::Local::now().naive_local()) {
            return Err(invalid(
                "Der Zeitpunkt der einmaligen Aufgabe muss in der Zukunft liegen.",
            ));
        }
        let executable = std::path::absolute(self.executable)?;
        let script = std::path::absolute(self.script)?;
        let profile = std::path::absolute(self.profile)?;
        check_file(&executable, "arco-net.exe")?;
        check_file(&script, "Das Netzwerkskript")?;
        task_path(&profile)?;
        let log = self.log.map(std::path::absolute).transpose()?;
        if let Some(log) = &log {
            if log == &script || log == &profile || log == &executable {
                return Err(invalid("Wähle eine eigene Protokolldatei für die Aufgabe."));
            }
            task_path(log)?;
        }
        let directory = script
            .parent()
            .ok_or_else(|| invalid("Der Skriptordner fehlt."))?;
        let mut args = format!(
            "--script {} --profile {}",
            argument(&script)?,
            argument(&profile)?
        );
        if let Some(log) = log {
            args.push_str(" --log-dir ");
            args.push_str(&argument(&log)?);
        }
        Ok(Launch {
            path: task_path(&executable)?.into(),
            directory: task_path(directory)?.into(),
            args: args.into(),
        })
    }
}

pub fn list() -> io::Result<Vec<Task>> {
    with_scheduler(|_, folder| unsafe {
        let registered = folder.GetTasks(0)?;
        let mut tasks = Vec::new();
        for index in 1..=registered.Count()? {
            let task = registered.get_Item(&VARIANT::from(index))?;
            let name = task.Name()?.to_string();
            if let Some(name) = name.strip_prefix(PREFIX)
                && let Some(logon) = owned_logon(&task)?
            {
                let state = match task.State()? {
                    TASK_STATE_DISABLED => "disabled",
                    TASK_STATE_QUEUED => "queued",
                    TASK_STATE_READY => "ready",
                    TASK_STATE_RUNNING => "running",
                    _ => "unknown",
                };
                tasks.push(Task {
                    name: name.to_owned(),
                    state,
                    last_result: task.LastTaskResult()?,
                    background: logon == TASK_LOGON_PASSWORD,
                });
            }
        }
        tasks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tasks)
    })
}

pub fn run(name: &str) -> io::Result<()> {
    operate(name, |_, _, task| unsafe {
        task.Run(&VARIANT::default()).map(|_| ())
    })
}

pub fn remove(name: &str) -> io::Result<()> {
    operate(name, |folder, name, _| unsafe {
        folder.DeleteTask(name, 0)
    })
}

fn operate(
    name: &str,
    op: impl FnOnce(&ITaskFolder, &BSTR, &IRegisteredTask) -> ComResult<()>,
) -> io::Result<()> {
    let name = full_name(name)?;
    with_scheduler(|_, folder| unsafe {
        let task = folder.GetTask(&name)?;
        if owned_logon(&task)?.is_none() {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x80070005u32 as i32),
                "Diese Aufgabe gehört nicht zu Arco Commander.",
            ));
        }
        op(folder, &name, &task)
    })
}

fn owned_logon(task: &IRegisteredTask) -> ComResult<Option<TASK_LOGON_TYPE>> {
    let mut source = BSTR::new();
    unsafe {
        let definition = task.Definition()?;
        definition.RegistrationInfo()?.Source(&mut source)?;
        if source.to_string() != SOURCE {
            return Ok(None);
        }
        let mut logon = TASK_LOGON_TYPE::default();
        definition.Principal()?.LogonType(&mut logon)?;
        Ok(Some(logon))
    }
}

fn current_account() -> io::Result<BSTR> {
    let mut name = [0u16; 1024];
    let mut length = name.len() as u32;
    // This reads the current process identity and avoids taking a different account from the script.
    if !unsafe { GetUserNameExW(NameSamCompatible, name.as_mut_ptr(), &mut length) } {
        return Err(io::Error::last_os_error());
    }
    if length == 0 || length as usize >= name.len() {
        return Err(invalid(
            "Das aktuelle Windows-Konto konnte nicht ermittelt werden.",
        ));
    }
    let name = &name[..length as usize];
    Ok(BSTR::from_wide(name.strip_suffix(&[0]).unwrap_or(name)))
}

struct TaskPassword {
    variant: VARIANT,
    buffer: *mut u16,
    length: usize,
}

impl TaskPassword {
    fn new(password: &str) -> Self {
        let encoded = Zeroizing::new(password.encode_utf16().collect::<Vec<_>>());
        let buffer = BSTR::from_wide(&encoded).into_raw().cast_mut();
        // VARIANT takes ownership of this BSTR. No clones of the VARIANT are made.
        let variant = VARIANT::from(unsafe { BSTR::from_raw(buffer) });
        Self {
            variant,
            buffer,
            length: encoded.len(),
        }
    }
}

impl Drop for TaskPassword {
    fn drop(&mut self) {
        // The COM call has returned. Clear the data while the owned VARIANT still holds the BSTR.
        unsafe { std::slice::from_raw_parts_mut(self.buffer, self.length) }.zeroize();
    }
}

fn full_name(name: &str) -> io::Result<BSTR> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 80
        || !name
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, ' ' | '_' | '-'))
    {
        return Err(invalid(
            "Verwende bis zu 80 Buchstaben, Ziffern, Leerzeichen, Bindestriche oder Unterstriche für den Aufgabennamen.",
        ));
    }
    Ok(format!("{PREFIX}{name}").into())
}

struct Launch {
    path: BSTR,
    args: BSTR,
    directory: BSTR,
}

fn definition(
    service: &ITaskService,
    launch: &Launch,
    mode: TASK_LOGON_TYPE,
    account: Option<&BSTR>,
    schedule: Trigger,
) -> ComResult<ITaskDefinition> {
    unsafe {
        let definition = service.NewTask(0)?;
        definition
            .RegistrationInfo()?
            .SetSource(&BSTR::from(SOURCE))?;
        let principal = definition.Principal()?;
        principal.SetLogonType(mode)?;
        if let Some(account) = account {
            principal.SetUserId(account)?;
        }
        principal.SetRunLevel(TASK_RUNLEVEL_LUA)?;
        definition
            .Settings()?
            .SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
        let (kind, start) = match schedule {
            Trigger::Once(at) => (TASK_TRIGGER_TIME, Some(at)),
            Trigger::Logon => (TASK_TRIGGER_LOGON, None),
            Trigger::Startup => (TASK_TRIGGER_BOOT, None),
            Trigger::Daily(at) | Trigger::Weekly(at, _) | Trigger::Monthly(at, _) => {
                let kind = match schedule {
                    Trigger::Weekly(..) => TASK_TRIGGER_WEEKLY,
                    Trigger::Monthly(..) => TASK_TRIGGER_MONTHLY,
                    _ => TASK_TRIGGER_DAILY,
                };
                (kind, Some(chrono::Local::now().date_naive().and_time(at)))
            }
        };
        let trigger = definition.Triggers()?.Create(kind)?;
        if let Some(start) = start {
            trigger.SetStartBoundary(&BSTR::from(start.format("%Y-%m-%dT%H:%M:%S").to_string()))?;
        }
        match schedule {
            Trigger::Daily(_) => trigger.cast::<IDailyTrigger>()?.SetDaysInterval(1)?,
            Trigger::Weekly(_, days) => {
                let weekly = trigger.cast::<IWeeklyTrigger>()?;
                weekly.SetWeeksInterval(1)?;
                weekly.SetDaysOfWeek(days)?;
            }
            Trigger::Monthly(_, days) => {
                let monthly = trigger.cast::<IMonthlyTrigger>()?;
                monthly.SetMonthsOfYear(0x0fff)?;
                monthly.SetDaysOfMonth(days)?;
            }
            Trigger::Logon => trigger
                .cast::<ILogonTrigger>()?
                .SetUserId(account.expect("account for logon trigger"))?,
            Trigger::Once(_) | Trigger::Startup => (),
        }
        let action = definition
            .Actions()?
            .Create(TASK_ACTION_EXEC)?
            .cast::<IExecAction>()?;
        action.SetPath(&launch.path)?;
        action.SetArguments(&launch.args)?;
        action.SetWorkingDirectory(&launch.directory)?;
        Ok(definition)
    }
}

fn with_scheduler<T>(
    operation: impl FnOnce(&ITaskService, &ITaskFolder) -> ComResult<T>,
) -> io::Result<T> {
    // Balance successful COM initialization. An STA host may already have initialized COM.
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    const CHANGED_MODE: windows::core::HRESULT = windows::core::HRESULT(0x80010106u32 as i32);
    if init.is_err() && init != CHANGED_MODE {
        return Err(io::Error::other(windows::core::Error::from_hresult(init)));
    }
    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    let _apartment = Apartment(init.is_ok());
    let dispatch = || unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let empty = VARIANT::default();
        service.Connect(&empty, &empty, &empty, &empty)?;
        let root = service.GetFolder(&BSTR::from("\\"))?;
        operation(&service, &root)
    };
    dispatch().map_err(io::Error::other)
}

fn check_file(path: &Path, label: &str) -> io::Result<()> {
    task_path(path)?;
    if !path.metadata().is_ok_and(|meta| meta.is_file()) {
        return Err(invalid(&format!("{label} muss als Datei vorhanden sein.")));
    }
    Ok(())
}

fn task_path(path: &Path) -> io::Result<&str> {
    path.to_str()
        .filter(|path| !path.contains(['%', '"']) && !path.chars().any(char::is_control))
        .ok_or_else(|| invalid("Der Aufgabenpfad enthält unzulässige Zeichen."))
}

fn argument(path: &Path) -> io::Result<String> {
    let path = task_path(path)?;
    let trailing_slashes = path
        .as_bytes()
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'\\')
        .count();
    Ok(format!("\"{path}{}\"", "\\".repeat(trailing_slashes)))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn once() -> Trigger {
        Trigger::parse("once", "2037-02-03T05:42").unwrap()
    }

    #[test]
    fn validates_calendar_trigger_arguments() {
        assert!(matches!(
            Trigger::parse("weekly", "05:42 Mo,Fri,So"),
            Ok(Trigger::Weekly(_, 35))
        ));
        assert!(matches!(
            Trigger::parse("monthly", "05:42 1,15,31"),
            Ok(Trigger::Monthly(_, 0x40004001))
        ));
        assert!(matches!(
            Trigger::parse("logon", "logon"),
            Ok(Trigger::Logon)
        ));
        assert!(
            Trigger::parse("startup", "startup")
                .unwrap()
                .requires_background()
        );
        for (kind, value) in [
            ("daily", "25:00"),
            ("weekly", "10:00"),
            ("weekly", "10:00 Mo,"),
            ("monthly", "10:00 32"),
            ("monthly", "10:00 0"),
            ("once", "2026-02-30T08:00"),
            ("logon", "10:00"),
            ("startup", "10:00"),
        ] {
            assert!(Trigger::parse(kind, value).is_err(), "{kind} {value}");
        }
    }

    #[test]
    fn rejects_task_names_that_could_address_other_tasks() {
        for name in ["", r"\Microsoft\Task", "%WINDIR%", "Ziel*"] {
            assert!(full_name(name).is_err());
        }
        assert_eq!(
            full_name("Sicherung Büro").unwrap().to_string(),
            "Arco Commander - Sicherung Büro"
        );
        assert_eq!(
            argument(Path::new(r"C:\Büro & Archiv\datei.txt")).unwrap(),
            "\"C:\\Büro & Archiv\\datei.txt\""
        );
    }

    #[test]
    #[ignore = "requires the local Windows Task Scheduler service"]
    fn reference_builds_definition_without_registering_or_running_task() {
        let launch = Launch {
            path: BSTR::from(r"C:\Tools\arco-net.exe"),
            args: BSTR::from(
                r#"--script "C:\Neutral & Lokal\transfer.txt" --profile "C:\Arco\profile.json""#,
            ),
            directory: BSTR::from(r"C:\Neutral & Lokal"),
        };
        with_scheduler(|service, _| unsafe {
            let task = definition(service, &launch, TASK_LOGON_INTERACTIVE_TOKEN, None, once())?;
            let mut policy = Default::default();
            let mut logon = Default::default();
            task.Settings()?.MultipleInstances(&mut policy)?;
            task.Principal()?.LogonType(&mut logon)?;
            assert_eq!(policy, TASK_INSTANCES_IGNORE_NEW);
            assert_eq!(logon, TASK_LOGON_INTERACTIVE_TOKEN);
            let mut xml = BSTR::new();
            task.XmlText(&mut xml)?;
            let xml = xml.to_string();
            assert!(xml.contains("2037-02-03T05:42:00"));
            assert!(xml.contains("arco-net.exe") && xml.contains("&amp; Lokal"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    #[ignore = "requires the local Windows Task Scheduler service"]
    fn reference_builds_background_definition_for_current_account() {
        let launch = Launch {
            path: BSTR::from(r"C:\Tools\arco-net.exe"),
            args: BSTR::from(r#"--script "C:\Test\script.txt""#),
            directory: BSTR::from(r"C:\Test"),
        };
        let account = current_account().unwrap();
        assert!(account.contains(&u16::from(b'\\')));
        assert!(!account.contains(&0));
        with_scheduler(|service, _| unsafe {
            let task = definition(
                service,
                &launch,
                TASK_LOGON_PASSWORD,
                Some(&account),
                once(),
            )?;
            let principal = task.Principal()?;
            let mut mode = TASK_LOGON_TYPE::default();
            principal.LogonType(&mut mode)?;
            let mut user = BSTR::new();
            principal.UserId(&mut user)?;
            assert_eq!(mode, TASK_LOGON_PASSWORD);
            assert_eq!(user, account);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    #[ignore = "uses a disposable disabled task in the local Windows Task Scheduler"]
    fn reference_registers_lists_and_removes_disabled_task() {
        use windows::Win32::System::TaskScheduler::TASK_DISABLE;
        let name = format!("Reference {}", unsafe {
            windows::Win32::System::Com::CoCreateGuid()
                .unwrap()
                .to_u128()
        });
        let id = full_name(&name).unwrap();
        let launch = Launch {
            path: BSTR::from(std::env::current_exe().unwrap().to_string_lossy().as_ref()),
            args: BSTR::from("--help"),
            directory: BSTR::new(),
        };
        with_scheduler(|service, folder| unsafe {
            let task = definition(service, &launch, TASK_LOGON_INTERACTIVE_TOKEN, None, once())?;
            task.Settings()?.SetEnabled(false.into())?;
            let empty = VARIANT::default();
            folder.RegisterTaskDefinition(
                &id,
                &task,
                TASK_CREATE.0 | TASK_DISABLE.0,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )?;
            Ok(())
        })
        .unwrap();
        let found = list();
        remove(&name).unwrap();
        let found = found.unwrap();
        assert!(
            found
                .iter()
                .any(|task| task.name == name && task.state == "disabled")
        );
        assert!(list().unwrap().iter().all(|task| task.name != name));
    }
}
