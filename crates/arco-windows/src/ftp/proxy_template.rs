use super::invalid_input;
use std::io;
use zeroize::Zeroizing;

const TELNET_DEFAULT: &str = r"connect %host %port\r\n";

#[derive(Clone)]
pub(super) struct Command {
    parts: Vec<Part>,
    pub user: bool,
    pub password: bool,
}

#[derive(Clone)]
enum Part {
    Text(Vec<u8>),
    Host,
    Port,
    User,
    Password,
}

impl Command {
    pub fn telnet(template: &str) -> io::Result<Self> {
        Self::parse(
            if template.is_empty() {
                TELNET_DEFAULT
            } else {
                template
            },
            false,
        )
    }

    pub fn argument(template: &str) -> io::Result<Self> {
        Self::parse(template, true)
    }

    fn parse(template: &str, windows_path: bool) -> io::Result<Self> {
        if template.len() > 4096 || template.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(invalid_input("Ungültige oder zu lange SSH-Proxy-Vorlage."));
        }
        let mut rest = template.as_bytes();
        let mut text = Vec::new();
        let mut parts = Vec::new();
        let mut user = false;
        let mut password = false;
        while let Some((&byte, tail)) = rest.split_first() {
            rest = tail;
            match byte {
                b'%' if rest.starts_with(b"%") => {
                    text.push(b'%');
                    rest = &rest[1..];
                }
                b'%' => {
                    let (name, part) = [
                        (b"host".as_slice(), Part::Host),
                        (b"port".as_slice(), Part::Port),
                        (b"user".as_slice(), Part::User),
                        (b"pass".as_slice(), Part::Password),
                    ]
                    .into_iter()
                    .find(|(name, _)| rest.starts_with(name))
                    .ok_or_else(|| {
                        invalid_input("Unbekannter Platzhalter in der SSH-Proxy-Vorlage.")
                    })?;
                    user |= matches!(part, Part::User);
                    password |= matches!(part, Part::Password);
                    if !text.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut text)));
                    }
                    parts.push(part);
                    rest = &rest[name.len()..];
                }
                b'\\'
                    if windows_path
                        && !rest.first().is_some_and(|value| {
                            matches!(value, b'n' | b'r' | b't' | b'\\' | b'x')
                        }) =>
                {
                    text.push(b'\\');
                }
                b'\\' => text.push(escape(&mut rest)?),
                _ => text.push(byte),
            }
        }
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        Ok(Self {
            parts,
            user,
            password,
        })
    }

    pub fn render(
        &self,
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        if self.user && user.is_empty() || self.password && password.is_empty() {
            return Err(invalid_input(
                "Ergänze die Zugangsdaten für den SSH-Proxy im Netzwerkprofil.",
            ));
        }
        let port = port.to_string();
        let mut bytes = Zeroizing::new(Vec::new());
        for part in &self.parts {
            bytes.extend_from_slice(match part {
                Part::Text(text) => text,
                Part::Host => host.as_bytes(),
                Part::Port => port.as_bytes(),
                Part::User => user.as_bytes(),
                Part::Password => password.as_bytes(),
            });
        }
        if bytes.len() > 8192 {
            return Err(invalid_input("Das SSH-Proxy-Kommando ist zu lang."));
        }
        Ok(bytes)
    }
}

fn escape(rest: &mut &[u8]) -> io::Result<u8> {
    let invalid = || invalid_input("Ungültige Escape-Sequenz in der SSH-Proxy-Vorlage.");
    let (&escaped, tail) = rest.split_first().ok_or_else(invalid)?;
    *rest = tail;
    match escaped {
        b'n' => Ok(b'\n'),
        b'r' => Ok(b'\r'),
        b't' => Ok(b'\t'),
        b'\\' => Ok(b'\\'),
        b'x' => {
            let digits = std::str::from_utf8(
                rest.get(..2)
                    .filter(|digits| digits.iter().all(u8::is_ascii_hexdigit))
                    .ok_or_else(invalid)?,
            )
            .map_err(|_| invalid())?;
            let byte = u8::from_str_radix(digits, 16).map_err(|_| invalid())?;
            *rest = &rest[2..];
            Ok(byte)
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telnet_proxy_uses_literal_credentials_and_expands_winscp_escapes() {
        let command = Command::telnet(r"%user\n%pass\x0d\nconnect %host\t%port %% \\").unwrap();
        assert_eq!(
            command
                .render("ftp.invalid", 2222, "üser", "%host")
                .unwrap()
                .as_slice(),
            "üser\n%host\r\nconnect ftp.invalid\t2222 % \\".as_bytes()
        );
        assert!(command.render("host", 22, "", "secret").is_err());
        assert!(Command::telnet(r"\x1-").is_err());
        assert!(Command::telnet(r"%wrong").is_err());
    }
}
