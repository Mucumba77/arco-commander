use std::io;

/// Retain explicit Windows quoting alongside CommandLineToArgvW's decoded
/// arguments. This distinguishes %Q from names the user enclosed in quotes.
pub(super) fn manual_quotes(raw: &str) -> io::Result<Vec<bool>> {
    if raw.encode_utf16().count() > 32767
        || raw
            .chars()
            .any(|character| character.is_control() && character != '\t')
    {
        return Err(io::Error::other(
            "Ungültige oder zu lange Windows-Parameterzeile.",
        ));
    }
    let mut flags = Vec::new();
    let (mut started, mut quoted, mut manual, mut slashes) = (false, false, false, 0usize);
    for character in raw.chars() {
        if matches!(character, ' ' | '\t') && !quoted {
            if started {
                flags.push(manual);
            }
            (started, manual, slashes) = (false, false, 0);
            continue;
        }
        started = true;
        if character == '"' {
            manual = true;
            if slashes % 2 == 0 {
                quoted = !quoted;
            }
        }
        slashes = if character == '\\' { slashes + 1 } else { 0 };
    }
    if started {
        flags.push(manual);
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tracks_manual_windows_quotes_without_losing_empty_and_escaped_arguments() {
        for (raw, flags) in [
            (
                r#"%Q %P%N "%P%N" /out="%P%N" """#,
                vec![false, false, true, true, true],
            ),
            (r#"a\"b "c\\" d"#, vec![true, true, false]),
            (r#" "a b"  c "unterminated"#, vec![true, false, true]),
        ] {
            assert_eq!(manual_quotes(raw).unwrap(), flags);
            assert_eq!(
                arco_windows::parse_windows_arguments(raw).unwrap().len(),
                flags.len()
            );
        }
        assert!(manual_quotes("a\nb").is_err());
    }
}
