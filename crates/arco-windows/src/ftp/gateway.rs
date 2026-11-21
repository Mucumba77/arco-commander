use super::{Session, Url, fxp::Control, invalid_input};
use arco_core::profile::FtpGateway;
use std::io;
use std::sync::atomic::AtomicBool;
use zeroize::Zeroizing;

impl Session {
    pub(super) fn gateway_login(
        &self,
        gateway: &Url,
        target: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<Control> {
        let connection = &self.connection;
        let mode = connection.ftp_gateway;
        let mut control = Control::connect_peer(self, gateway, connection.options.family, cancel)?;
        let host = target
            .host_str()
            .ok_or_else(|| invalid_input("FTP-Server fehlt."))?;
        let host = match target.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        let user = &connection.user;
        let proxy_user = &connection.proxy_user;
        let proxy_password = connection.proxy_password.as_str();
        match mode {
            FtpGateway::Site | FtpGateway::Open if !proxy_user.is_empty() => {
                login(&mut control, proxy_user, proxy_password, None, cancel)?;
            }
            FtpGateway::ProxyThenUser | FtpGateway::ProxyThenUserAtHost => {
                login(&mut control, proxy_user, proxy_password, None, cancel)?;
            }
            FtpGateway::ProxyAtHostThenUser => {
                login(
                    &mut control,
                    &format!("{proxy_user}@{host}"),
                    proxy_password,
                    None,
                    cancel,
                )?;
            }
            _ => {}
        }
        if let Some(verb) = match mode {
            FtpGateway::Site => Some("SITE"),
            FtpGateway::Open => Some("OPEN"),
            _ => None,
        } {
            control.send(&format!("{verb} {host}"), cancel)?;
            let (status, _) = control.reply(std::time::Duration::from_secs(30), cancel)?;
            if !(200..400).contains(&status) {
                return Err(io::Error::other(format!(
                    "FTP-Gateway meldet Status {status} bei {verb}."
                )));
            }
        }
        let target_user = match mode {
            FtpGateway::UserAtHost | FtpGateway::ProxyThenUserAtHost => format!("{user}@{host}"),
            FtpGateway::UserAtHostAndProxy => format!("{user}@{host} {proxy_user}"),
            FtpGateway::UserAtProxyAtHost => format!("{user}@{proxy_user}@{host}"),
            _ => user.clone(),
        };
        let needs_combined_password = mode == FtpGateway::UserAtProxyAtHost;
        let password = if needs_combined_password {
            Zeroizing::new(format!("{}@{proxy_password}", self.password.as_str()))
        } else {
            Zeroizing::new(self.password.as_str().to_owned())
        };
        let account = matches!(
            mode,
            FtpGateway::UserAtHostAndProxy | FtpGateway::UserAtProxyAtHost
        )
        .then_some(proxy_password);
        login(&mut control, &target_user, &password, account, cancel)?;
        Ok(control)
    }
}

fn login(
    control: &mut Control,
    user: &str,
    password: &str,
    account: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let (mut status, _) =
        control.command(&format!("USER {user}"), &[230, 331, 332], "USER", cancel)?;
    if status == 331 {
        let password = Zeroizing::new(format!("PASS {password}"));
        status = control.command(&password, &[230, 332], "PASS", cancel)?.0;
    }
    if status == 332 {
        let account = account.ok_or_else(|| {
            invalid_input("Das FTP-Gateway verlangt eine zusätzliche Kontokennung.")
        })?;
        let account = Zeroizing::new(format!("ACCT {account}"));
        control.command(&account, &[230, 202], "ACCT", cancel)?;
    }
    Ok(())
}
