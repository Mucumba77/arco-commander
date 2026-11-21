use arco_core::transfer_names::remote_leaf;
use arco_windows::ftp::{Session, Url};
use std::io;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    File,
    Files,
    Directory,
    Host,
    User,
    Port,
    Name,
    Url,
}

enum Part {
    Literal(String),
    Field(Field),
}

#[derive(Default)]
struct SessionFields<'a> {
    host: &'a str,
    user: &'a str,
    port: &'a str,
    name: &'a str,
    url: &'a str,
}

/// Renders a command and its arguments as POSIX shell words.
pub fn expand_for_session(
    template: &str,
    session: &Session,
    directory: &Url,
    profile: Option<&str>,
    files: &[&str],
) -> io::Result<Vec<String>> {
    let host = directory
        .host_str()
        .unwrap_or_default()
        .trim_matches(['[', ']']);
    let port = directory.port_or_known_default().unwrap_or(22).to_string();
    let mut url = directory.clone();
    url.set_password(None)
        .map_err(|()| invalid("Ungültige SSH-Sitzungsadresse."))?;
    url.set_username(session.username())
        .map_err(|()| invalid("Ungültige SSH-Sitzungsadresse."))?;
    url.set_query(None);
    url.set_fragment(None);
    let path = Session::directory_path(directory);
    let fields = SessionFields {
        host,
        user: session.username(),
        port: &port,
        name: profile.unwrap_or(host),
        url: url.as_str(),
    };
    expand(template, &path, files, &fields)
}

fn expand(
    template: &str,
    directory: &str,
    files: &[&str],
    fields: &SessionFields<'_>,
) -> io::Result<Vec<String>> {
    let parts = parse(template)?;
    let contains = |field| {
        parts
            .iter()
            .any(|part| matches!(part, Part::Field(found) if *found == field))
    };
    let one = contains(Field::File);
    let all = contains(Field::Files);
    if one && all {
        return Err(invalid(
            "Die Platzhalter ! und !& können nur getrennt verwendet werden.",
        ));
    }
    if one || all {
        if files.is_empty() || files.len() > 256 {
            return Err(invalid(
                "Die Befehlsvorlage benötigt 1 bis 256 markierte Einträge.",
            ));
        }
        for name in files {
            remote_leaf(name)?;
        }
    }
    let directory = if directory.ends_with('/') {
        directory.to_owned()
    } else {
        format!("{directory}/")
    };
    let directory = quote(&directory);
    let files: Vec<_> = if one || all {
        files
            .iter()
            .map(|name| quote(&format!("./{name}")))
            .collect()
    } else {
        Vec::new()
    };
    let list = if all { files.join(" ") } else { String::new() };
    let render = |file: &str| {
        let mut command = String::new();
        for part in &parts {
            if !command.is_empty() {
                command.push(' ');
            }
            let argument = match part {
                Part::Literal(text) => text,
                Part::Field(Field::File) => file,
                Part::Field(Field::Files) => &list,
                Part::Field(Field::Directory) => &directory,
                Part::Field(field) => {
                    let value = match field {
                        Field::Host => fields.host,
                        Field::User => fields.user,
                        Field::Port => fields.port,
                        Field::Name => fields.name,
                        Field::Url => fields.url,
                        Field::File | Field::Files | Field::Directory => unreachable!(),
                    };
                    command.push_str(&quote(value));
                    continue;
                }
            };
            command.push_str(argument);
        }
        if command.len() > 16 * 1024 {
            return Err(invalid("Der ausgefüllte SSH-Befehl ist zu lang."));
        }
        Ok(command)
    };
    if one {
        files.iter().map(|input| render(input)).collect()
    } else {
        Ok(vec![render("")?])
    }
}

pub fn validate(template: &str) -> io::Result<()> {
    validate_selection(template, &["entry"])
}

pub fn validate_selection(template: &str, files: &[&str]) -> io::Result<()> {
    expand(template, "/", files, &SessionFields::default()).map(|_| ())
}

fn parse(template: &str) -> io::Result<Vec<Part>> {
    if template.trim().is_empty() || template.len() > 16 * 1024 || template.contains('\0') {
        return Err(invalid("Die SSH-Befehlsvorlage fehlt oder ist ungültig."));
    }
    let mut parts = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut delimiter = None;
    let mut escaped = false;
    let mut shell_operator = false;
    for ch in template.chars() {
        if escaped {
            if delimiter == Some('"') && !matches!(ch, '"' | '\\' | '$' | '`' | '\n') {
                word.push('\\');
            }
            if ch != '\n' {
                word.push(ch);
            }
            escaped = false;
            continue;
        }
        if let Some(quote) = delimiter {
            match ch {
                ch if ch == quote => delimiter = None,
                '\\' if quote == '"' => escaped = true,
                _ => word.push(ch),
            }
            continue;
        }
        if ch.is_ascii_whitespace() {
            if started {
                parts.push(part(&mut word, shell_operator)?);
                shell_operator = false;
                started = false;
            }
            continue;
        }
        started = true;
        match ch {
            '\'' | '"' => delimiter = Some(ch),
            '\\' => escaped = true,
            _ => {
                shell_operator |= matches!(ch, ';' | '|' | '(' | ')' | '<' | '>' | '&' | '`' | '$');
                word.push(ch);
            }
        }
    }
    if delimiter.is_some() || escaped {
        return Err(invalid(
            "Die SSH-Befehlsvorlage enthält ein offenes Anführungszeichen oder Escape-Zeichen.",
        ));
    }
    if started {
        parts.push(part(&mut word, shell_operator)?);
    }
    if !matches!(parts.first(), Some(Part::Literal(program)) if program != "''") {
        return Err(invalid(
            "Die SSH-Befehlsvorlage benötigt einen festen Programmnamen.",
        ));
    }
    Ok(parts)
}

fn part(word: &mut String, shell_operator: bool) -> io::Result<Part> {
    let result = match word.as_str() {
        "!&" => Part::Field(Field::Files),
        "!" => Part::Field(Field::File),
        "!/" => Part::Field(Field::Directory),
        "!@" => Part::Field(Field::Host),
        "!U" => Part::Field(Field::User),
        "!#" => Part::Field(Field::Port),
        "!N" => Part::Field(Field::Name),
        "!S" => Part::Field(Field::Url),
        "!!" => Part::Literal(quote("!")),
        _ if shell_operator => {
            return Err(invalid(
                "Shell-Operatoren sind im Modus Direkter Befehl verfügbar.",
            ));
        }
        _ => Part::Literal(quote(word)),
    };
    word.clear();
    Ok(result)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_names_and_directory_as_quoted_arguments() {
        assert_eq!(
            expand(
                "printf '%s' \"!\" !/",
                "/source's folder",
                &["a'b; $x.txt", "-q.txt"],
                &SessionFields::default()
            )
            .unwrap(),
            [
                "'printf' '%s' './a'\\''b; $x.txt' '/source'\\''s folder/'",
                "'printf' '%s' './-q.txt' '/source'\\''s folder/'",
            ]
        );
        assert_eq!(
            expand(
                "echo !& !!",
                "/",
                &["name one", "name two"],
                &SessionFields::default()
            )
            .unwrap(),
            ["'echo' './name one' './name two' '!'"]
        );
    }

    #[test]
    fn expands_session_fields_without_secrets_or_url_parameters() {
        use arco_windows::ftp::{Connection, Security};

        let session = Connection::new("sftp://example.test:2221/work/", "arco", Security::Sftp)
            .unwrap()
            .with_ssh("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", None)
            .unwrap()
            .authenticate("fixture-password")
            .unwrap();
        let mut url = session.initial_directory();
        url.set_username("previous").unwrap();
        url.set_password(Some("embedded")).unwrap();
        url.set_query(Some("credential=hidden"));
        url.set_fragment(Some("hidden"));
        assert_eq!(
            expand_for_session(
                "echo !@ !U !# !N !S",
                &session,
                &url,
                Some("Team's profile"),
                &[]
            )
            .unwrap(),
            [
                "'echo' 'example.test' 'arco' '2221' 'Team'\\''s profile' 'sftp://arco@example.test:2221/work/'"
            ]
        );
        assert_eq!(
            expand_for_session("echo !N", &session, &url, None, &[]).unwrap(),
            ["'echo' 'example.test'"]
        );
    }

    #[test]
    fn rejects_mixed_placeholders_and_shell_operators_before_execution() {
        for template in ["echo ! !&", "! file", "echo ! | cat", "echo 'open quote"] {
            assert!(
                validate_selection(template, &["file.txt"]).is_err(),
                "{template}"
            );
        }
        assert!(validate_selection("echo !", &[]).is_err());
        assert!(validate_selection("echo !", &["../outside"]).is_err());
    }
}
