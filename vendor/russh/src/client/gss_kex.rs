//! Arco's client-side Kerberos transport extension (RFC 4462 and RFC 8732).

use std::fmt;
use std::io;
use std::sync::Arc;

use bytes::Bytes;
use ssh_encoding::{Decode, Encode};
use ssh_key::PublicKey;

use crate::kex::{self, KexAlgorithm, KexAlgorithmImplementor};
use crate::keys::key::parse_public_key;
use crate::negotiation::Preferred;
use crate::parsing::ensure_end;
use crate::session::Exchange;
use crate::sshbuffer::PacketWriter;
use crate::{CryptoVec, Error};

const INIT: u8 = 30;
const CONTINUE: u8 = 31;
const COMPLETE: u8 = 32;
const HOSTKEY: u8 = 33;
const ERROR: u8 = 34;
const MAX_TOKEN: usize = 1024 * 1024;
const MAX_ROUNDS: u8 = 16;

/// A step in the Kerberos initiator's security context establishment.
pub struct GssKexStep {
    /// Whether the service principal was mutually authenticated with integrity.
    pub complete: bool,
    /// Output token to send to the server, if any.
    pub token: Vec<u8>,
}

/// A Kerberos V5 initiator. Each exchange needs a fresh context bound to the
/// configured service principal. Implementations must verify mutual
/// authentication and integrity and must not delegate credentials.
pub trait GssKexContext: Send {
    /// Start the context or consume the next server token.
    fn step(&mut self, token: Option<&[u8]>) -> io::Result<GssKexStep>;
    /// Verify the peer's GSS MIC over the exchange hash.
    fn verify_mic(&mut self, hash: &[u8], mic: &[u8]) -> io::Result<()>;
    /// Sign the RFC 4462 user authentication request using the initial context.
    fn sign_mic(&mut self, data: &[u8]) -> io::Result<Vec<u8>>;
}

type Factory = dyn Fn() -> io::Result<Box<dyn GssKexContext>> + Send + Sync;

/// A client-only RFC 8732 Kerberos key exchange configuration.
#[derive(Clone)]
pub struct GssKex {
    factory: Arc<Factory>,
    allow_missing_host_key: bool,
}

impl GssKex {
    /// Require Kerberos KEX. Allow omission of the host key only when the
    /// configured service principal is itself the intended trust anchor.
    pub fn new(
        factory: impl Fn() -> io::Result<Box<dyn GssKexContext>> + Send + Sync + 'static,
        allow_missing_host_key: bool,
    ) -> Self {
        Self {
            factory: Arc::new(factory),
            allow_missing_host_key,
        }
    }

    pub(crate) fn preferred(&self, mut preferred: Preferred) -> Preferred {
        preferred.kex = vec![
            kex::GSS_CURVE25519,
            kex::GSS_GROUP16,
            kex::GSS_GROUP14,
            kex::EXTENSION_SUPPORT_AS_CLIENT,
            kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
        ]
        .into();
        preferred
    }

    pub(crate) fn allows_missing_host_key(&self) -> bool {
        self.allow_missing_host_key
    }
}

impl fmt::Debug for GssKex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GssKex")
            .field("allow_missing_host_key", &self.allow_missing_host_key)
            .finish_non_exhaustive()
    }
}

pub(crate) struct ExchangeState {
    context: Box<dyn GssKexContext>,
    complete: bool,
    rounds: u8,
    host_key: Option<Bytes>,
    allow_missing_host_key: bool,
}

impl fmt::Debug for ExchangeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GSS exchange").finish_non_exhaustive()
    }
}

impl ExchangeState {
    pub(super) fn start(
        provider: &GssKex,
        kex: &mut KexAlgorithm,
        exchange: &mut Exchange,
        output: &mut PacketWriter,
    ) -> Result<Self, Error> {
        let mut context = (provider.factory)()?;
        let step = context.step(None)?;
        validate_token(&step.token)?;
        // Generate the ephemeral key using the corresponding non-GSS primitive.
        // GSS adds the initiator token before the very same encoded public key.
        let mut public = Vec::new();
        kex.client_dh(&mut exchange.client_ephemeral, &mut public)?;
        let public = public.get(1..).ok_or(Error::KexInit)?;
        output.write_packet(|w| {
            INIT.encode(w)?;
            step.token.encode(w)?;
            w.extend_from_slice(public);
            Ok(())
        })?;
        Ok(Self {
            context,
            complete: step.complete,
            rounds: 1,
            host_key: None,
            allow_missing_host_key: provider.allows_missing_host_key(),
        })
    }

    pub(super) fn receive(
        &mut self,
        packet: &[u8],
        kex: &mut KexAlgorithm,
        exchange: &mut Exchange,
        output: &mut PacketWriter,
    ) -> Result<Option<(Vec<u8>, Option<PublicKey>)>, Error> {
        let mut reader = packet;
        match u8::decode(&mut reader)? {
            HOSTKEY if self.host_key.is_none() => {
                let key = Bytes::decode(&mut reader)?;
                validate_token(&key)?;
                ensure_end(&reader)?;
                self.host_key = Some(key);
                Ok(None)
            }
            CONTINUE => {
                let token = Bytes::decode(&mut reader)?;
                ensure_end(&reader)?;
                let step = self.step(&token)?;
                validate_token(&step.token)?;
                output.write_packet(|w| {
                    CONTINUE.encode(w)?;
                    step.token.encode(w)?;
                    Ok(())
                })?;
                Ok(None)
            }
            COMPLETE => {
                let ephemeral = Bytes::decode(&mut reader)?;
                let mic = Bytes::decode(&mut reader)?;
                validate_token(&mic)?;
                if u8::decode(&mut reader)? != 0 {
                    let token = Bytes::decode(&mut reader)?;
                    ensure_end(&reader)?;
                    let step = self.step(&token)?;
                    if !step.complete || !step.token.is_empty() {
                        return Err(Error::KexInit);
                    }
                }
                ensure_end(&reader)?;
                if !self.complete {
                    return Err(Error::KexInit);
                }
                let host_key_blob = self.host_key.as_deref().unwrap_or_default();
                if host_key_blob.is_empty() && !self.allow_missing_host_key {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Der Server hat für den GSSAPI-Schlüsselaustausch keinen SSH-Hostschlüssel übertragen. Der hinterlegte Fingerabdruck konnte nicht geprüft werden.",
                    ).into());
                }
                exchange.server_ephemeral = ephemeral.to_vec();
                kex.compute_shared_secret(&ephemeral)?;
                let mut encoded_key = Vec::new();
                host_key_blob.encode(&mut encoded_key)?;
                let hash =
                    kex.compute_exchange_hash(&encoded_key, exchange, &mut CryptoVec::new())?;
                self.context.verify_mic(&hash, &mic)?;
                let host_key = self.host_key.as_deref().map(parse_public_key).transpose()?;
                Ok(Some((hash, host_key)))
            }
            ERROR => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Der Server hat den GSSAPI-Schlüsselaustausch abgelehnt.",
            )
            .into()),
            _ => Err(Error::KexInit),
        }
    }

    pub(super) fn auth_mic(&mut self, data: &[u8]) -> Result<Vec<u8>, Error> {
        let mic = self.context.sign_mic(data)?;
        validate_token(&mic)?;
        Ok(mic)
    }

    fn step(&mut self, token: &[u8]) -> Result<GssKexStep, Error> {
        validate_token(token)?;
        if self.complete || self.rounds >= MAX_ROUNDS {
            return Err(Error::KexInit);
        }
        self.rounds += 1;
        let step = self.context.step(Some(token))?;
        self.complete = step.complete;
        Ok(step)
    }
}

fn validate_token(token: &[u8]) -> Result<(), Error> {
    if !(1..=MAX_TOKEN).contains(&token.len()) {
        return Err(Error::KexInit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        expected_hash: Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl GssKexContext for Fixture {
        fn step(&mut self, token: Option<&[u8]>) -> io::Result<GssKexStep> {
            match token {
                None => Ok(GssKexStep {
                    complete: false,
                    token: b"initial".to_vec(),
                }),
                Some(b"challenge") => Ok(GssKexStep {
                    complete: false,
                    token: b"response".to_vec(),
                }),
                Some(b"final") => Ok(GssKexStep {
                    complete: true,
                    token: vec![],
                }),
                _ => Err(io::Error::other("Unexpected fixture token")),
            }
        }

        fn verify_mic(&mut self, hash: &[u8], mic: &[u8]) -> io::Result<()> {
            if hash == *self.expected_hash.lock().unwrap() && mic == b"valid server MIC" {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Invalid fixture MIC",
                ))
            }
        }

        fn sign_mic(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
            Ok(data.to_vec())
        }
    }

    fn complete_exchange(
        name: kex::Name,
        allow_missing_key: bool,
        host_key: Option<&[u8]>,
        mic: &[u8],
        final_token: Option<&[u8]>,
    ) -> Result<Option<PublicKey>, Error> {
        let expected_hash = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = GssKex::new(
            {
                let expected_hash = expected_hash.clone();
                move || {
                    Ok(Box::new(Fixture {
                        expected_hash: expected_hash.clone(),
                    }))
                }
            },
            allow_missing_key,
        );
        let kind = kex::KEXES
            .get(&kex::gss_base_algorithm(name).unwrap())
            .unwrap();
        let mut client_kex = kind.make();
        let mut exchange = Exchange::new(b"SSH-2.0-client", b"SSH-2.0-server");
        exchange.client_kex_init = Bytes::from_static(b"client exchange proposal");
        exchange.server_kex_init = Bytes::from_static(b"server exchange proposal");
        let mut writer = PacketWriter::clear();
        let mut state =
            ExchangeState::start(&provider, &mut client_kex, &mut exchange, &mut writer)?;

        if let Some(host_key) = host_key {
            let mut packet = vec![HOSTKEY];
            host_key.encode(&mut packet)?;
            assert!(
                state
                    .receive(&packet, &mut client_kex, &mut exchange, &mut writer)?
                    .is_none()
            );
        }
        let mut packet = vec![CONTINUE];
        b"challenge".as_slice().encode(&mut packet)?;
        assert!(
            state
                .receive(&packet, &mut client_kex, &mut exchange, &mut writer)?
                .is_none()
        );

        let mut server = kind.make();
        let mut server_exchange = exchange.clone();
        let mut normal_init = vec![30];
        server_exchange.client_ephemeral.encode(&mut normal_init)?;
        server.server_dh(&mut server_exchange, &normal_init)?;
        let mut encoded_key = Vec::new();
        host_key.unwrap_or_default().encode(&mut encoded_key)?;
        *expected_hash.lock().unwrap() =
            server.compute_exchange_hash(&encoded_key, &server_exchange, &mut CryptoVec::new())?;

        let mut packet = vec![COMPLETE];
        server_exchange.server_ephemeral.encode(&mut packet)?;
        mic.encode(&mut packet)?;
        u8::from(final_token.is_some()).encode(&mut packet)?;
        if let Some(final_token) = final_token {
            final_token.encode(&mut packet)?;
        }
        let (hash, host_key) = state
            .receive(&packet, &mut client_kex, &mut exchange, &mut writer)?
            .expect("complete exchange");
        assert_eq!(hash, *expected_hash.lock().unwrap());
        assert_eq!(
            state.auth_mic(b"bound authentication request")?,
            b"bound authentication request"
        );
        Ok(host_key)
    }

    #[test]
    fn accepts_verified_sha2_exchange_with_and_without_host_key() {
        let mut host_key = Vec::new();
        "ssh-ed25519".encode(&mut host_key).unwrap();
        let key = [42; 32];
        key.as_slice().encode(&mut host_key).unwrap();
        for name in [kex::GSS_CURVE25519, kex::GSS_GROUP14, kex::GSS_GROUP16] {
            let observed = complete_exchange(
                name,
                false,
                Some(&host_key),
                b"valid server MIC",
                Some(b"final"),
            )
            .unwrap()
            .unwrap();
            assert_eq!(observed.to_bytes().unwrap(), host_key);
        }
        assert!(
            complete_exchange(
                kex::GSS_CURVE25519,
                true,
                None,
                b"valid server MIC",
                Some(b"final")
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn rejects_bad_mic_missing_pin_and_incomplete_context() {
        let algorithm = kex::GSS_CURVE25519;
        for (allow_missing, mic, token) in [
            (true, b"wrong MIC".as_slice(), Some(b"final".as_slice())),
            (
                false,
                b"valid server MIC".as_slice(),
                Some(b"final".as_slice()),
            ),
            (true, b"valid server MIC".as_slice(), None),
        ] {
            assert!(complete_exchange(algorithm, allow_missing, None, mic, token).is_err());
        }
    }

    #[test]
    fn proposal_is_gss_only_and_null_requires_explicit_trust() {
        use crate::kex::KexCause;
        use crate::negotiation::{Client, Select, write_kex_with_null};

        let provider = GssKex::new(|| unreachable!(), true);
        let preferred = provider.preferred(Preferred::DEFAULT);
        assert_eq!(preferred.kex.len(), 5);
        assert!(
            preferred
                .kex
                .iter()
                .take(3)
                .all(|name| kex::gss_base_algorithm(*name).is_some())
        );
        let mut peer = preferred.clone();
        peer.key.to_mut().clear();
        let packet = write_kex_with_null(&peer, &mut PacketWriter::clear(), None, true).unwrap();
        let names =
            Client::read_kex_with_null(&packet, &preferred, None, None, &KexCause::Initial, true)
                .unwrap();
        assert!(names.key.is_none());
        assert!(Client::read_kex(&packet, &preferred, None, None, &KexCause::Initial).is_err());
    }
}
