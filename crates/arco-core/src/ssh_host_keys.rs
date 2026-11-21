use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use std::{collections::BTreeSet, fmt, io};

const KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
];
const MAX_PINS: usize = 32;

/// Alternative keys explicitly trusted for an SSH endpoint.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostKeyPins(BTreeSet<[u8; 32]>);

impl HostKeyPins {
    pub fn parse(value: &str) -> io::Result<Self> {
        let mut pins = BTreeSet::new();
        for (index, pin) in value.split(';').enumerate() {
            if index >= MAX_PINS || pin.len() > 128 {
                return Err(invalid());
            }
            pins.insert(parse_pin(pin.trim()).ok_or_else(invalid)?);
        }
        Ok(Self(pins))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, digest: &[u8]) -> bool {
        self.0.contains(digest)
    }

    pub fn is_subset(&self, other: &Self) -> bool {
        self.0.is_subset(&other.0)
    }

    pub fn retain_shared(&mut self, other: &Self) {
        self.0.retain(|pin| other.0.contains(pin));
    }

    pub fn encoded(&self) -> impl Iterator<Item = String> + '_ {
        self.0.iter().map(|digest| STANDARD_NO_PAD.encode(digest))
    }
}

impl fmt::Display for HostKeyPins {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, digest) in self.encoded().enumerate() {
            if index > 0 {
                formatter.write_str(";")?;
            }
            write!(formatter, "SHA256:{digest}")?;
        }
        Ok(())
    }
}

fn parse_pin(pin: &str) -> Option<[u8; 32]> {
    let digest = if let Some(digest) = pin.strip_prefix("SHA256:") {
        digest
    } else if let Some(digest) = KEY_TYPES
        .iter()
        .find_map(|kind| pin.strip_prefix(kind)?.strip_prefix('-'))
    {
        digest
    } else {
        // WinSCP also displays the algorithm and bit count before a digest.
        let mut fields = pin.split_ascii_whitespace();
        if !KEY_TYPES.contains(&fields.next()?) || fields.next()?.parse::<u32>().ok()? == 0 {
            return None;
        }
        let digest = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        digest.strip_prefix("SHA256:").unwrap_or(digest)
    };
    let digest = digest.strip_suffix('=').unwrap_or(digest);
    STANDARD_NO_PAD
        .decode(digest)
        .or_else(|_| URL_SAFE_NO_PAD.decode(digest))
        .ok()?
        .try_into()
        .ok()
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Erwartet werden bis zu 32 gültige SHA256-Hostschlüssel. Trenne mehrere Fingerabdrücke durch Semikolon.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_and_winscp_pins_without_accepting_partial_input() {
        let first = STANDARD_NO_PAD.encode([1; 32]);
        let second = URL_SAFE_NO_PAD.encode([255; 32]);
        let canonical = format!(
            "SHA256:{first};SHA256:{}",
            STANDARD_NO_PAD.encode([255; 32]),
        );
        let input = format!("ssh-ed25519-{second}; ssh-rsa 2048 SHA256:{first}=; SHA256:{first}");
        let pins = HostKeyPins::parse(&input).unwrap();
        assert_eq!(pins.to_string(), canonical);
        assert!(pins.contains(&[1; 32]));
        assert!(pins.contains(&[255; 32]));
        assert!(!pins.contains(&[0; 32]));
        for invalid in [
            String::new(),
            format!("SHA256:{first};"),
            format!("SHA256:{first};*"),
            format!("SHA256:{first}=="),
            format!("unknown 2048 {first}"),
        ] {
            assert!(HostKeyPins::parse(&invalid).is_err(), "{invalid}");
        }
    }
}
