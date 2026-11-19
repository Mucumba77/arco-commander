use russh::client::{AuthResult, GssKex, GssKexContext, GssKexStep, Handle, Handler};
use russh::{GssapiAuthenticator, GssapiStep, SendError};
use std::io;
use std::ptr::null;
use windows_sys::Win32::Foundation::{
    SEC_E_OK, SEC_I_COMPLETE_AND_CONTINUE, SEC_I_COMPLETE_NEEDED, SEC_I_CONTINUE_NEEDED,
};
use windows_sys::Win32::Security::Authentication::Identity::{
    AcquireCredentialsHandleW, CompleteAuthToken, DeleteSecurityContext, FreeCredentialsHandle,
    ISC_REQ_DELEGATE, ISC_REQ_INTEGRITY, ISC_REQ_MUTUAL_AUTH, ISC_RET_DELEGATE, ISC_RET_INTEGRITY,
    ISC_RET_MUTUAL_AUTH, InitializeSecurityContextW, MakeSignature, QueryContextAttributesW,
    SECBUFFER_DATA, SECBUFFER_TOKEN, SECBUFFER_VERSION, SECPKG_ATTR_SIZES, SECPKG_CRED_OUTBOUND,
    SECURITY_NATIVE_DREP, SecBuffer, SecBufferDesc, SecPkgContext_Sizes, VerifySignature,
};
use windows_sys::Win32::Security::Credentials::SecHandle;

const KERBEROS_OID: &[u8] = &[
    0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02,
];
const MAX_TOKEN: usize = 1024 * 1024;

pub(super) async fn authenticate(
    session: &mut Handle<impl Handler>,
    user: &str,
    principal: &str,
    delegate: bool,
) -> io::Result<AuthResult> {
    let mut kerberos = Kerberos {
        principal: principal.encode_utf16().chain([0]).collect(),
        context: None,
        complete: false,
        delegate,
    };
    session
        .authenticate_gssapi_with_mic(user, vec![KERBEROS_OID.to_vec()], &mut kerberos)
        .await
        .map_err(|error| error.0)
}

pub(super) fn key_exchange(principal: &str, allow_missing_host_key: bool) -> GssKex {
    let principal: Vec<u16> = principal.encode_utf16().chain([0]).collect();
    GssKex::new(
        move || {
            let context = tokio::task::block_in_place(Context::new)?;
            Ok(Box::new(KeyExchange {
                principal: principal.clone(),
                context,
            }))
        },
        allow_missing_host_key,
    )
}

struct KeyExchange {
    principal: Vec<u16>,
    context: Context,
}

impl GssKexContext for KeyExchange {
    fn step(&mut self, token: Option<&[u8]>) -> io::Result<GssKexStep> {
        tokio::task::block_in_place(|| {
            let (complete, token) =
                self.context
                    .advance(&self.principal, token.map(<[u8]>::to_vec), false)?;
            Ok(GssKexStep { complete, token })
        })
    }

    fn verify_mic(&mut self, hash: &[u8], mic: &[u8]) -> io::Result<()> {
        let mut hash = hash.to_vec();
        let mut mic = mic.to_vec();
        let mut buffers = [
            buffer(SECBUFFER_DATA, &mut hash),
            buffer(SECBUFFER_TOKEN, &mut mic),
        ];
        let mut quality = 0;
        // SAFETY: The mutually authenticated context and both buffers are
        // live for the call. SSPI verifies the MIC using this context's keys.
        let status = unsafe {
            VerifySignature(
                &self.context.context,
                &descriptor(&mut buffers),
                0,
                &mut quality,
            )
        };
        check(status, "Kerberos-Prüfung des SSH-Schlüsselaustauschs")
    }

    fn sign_mic(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        self.context.signature(data.to_vec())
    }
}

struct Kerberos {
    principal: Vec<u16>,
    context: Option<Context>,
    complete: bool,
    delegate: bool,
}

struct AuthError(io::Error);

impl From<SendError> for AuthError {
    fn from(error: SendError) -> Self {
        Self(io::Error::other(error))
    }
}

impl GssapiAuthenticator for Kerberos {
    type Error = AuthError;

    async fn gssapi_step(
        &mut self,
        mechanism: Option<Vec<u8>>,
        token: Option<Vec<u8>>,
        mic_data: Vec<u8>,
    ) -> Result<GssapiStep, AuthError> {
        if self.complete
            || match self.context {
                None => mechanism.as_deref() != Some(KERBEROS_OID) || token.is_some(),
                Some(_) => mechanism.is_some() || token.as_ref().is_none_or(Vec::is_empty),
            }
            || token.as_ref().is_some_and(|token| token.len() > MAX_TOKEN)
        {
            return Err(AuthError(io::Error::new(
                io::ErrorKind::InvalidData,
                "Der SSH-Server hat einen ungültigen Kerberos-Austausch eingeleitet.",
            )));
        }
        let context = self.context.take();
        let principal = self.principal.clone();
        let delegate = self.delegate;
        let (context, result) = tokio::task::spawn_blocking(move || {
            let mut context = match context {
                Some(context) => context,
                None => Context::new()?,
            };
            let (complete, token) = context.advance(&principal, token, delegate)?;
            let result = if complete {
                GssapiStep::Complete {
                    token: (!token.is_empty()).then_some(token),
                    mic: Some(context.signature(mic_data)?),
                }
            } else {
                GssapiStep::Continue { token }
            };
            Ok::<_, io::Error>((context, result))
        })
        .await
        .map_err(|error| AuthError(io::Error::other(error)))?
        .map_err(AuthError)?;
        self.context = Some(context);
        self.complete = matches!(result, GssapiStep::Complete { .. });
        Ok(result)
    }
}

struct Context {
    credential: SecHandle,
    context: SecHandle,
}

impl Context {
    fn new() -> io::Result<Self> {
        let mut credential = invalid_handle();
        let mut expires = 0;
        // SAFETY: These buffers live for the SSPI call. Null auth data selects the
        // current Windows logon session and the Kerberos package alone.
        let result = unsafe {
            AcquireCredentialsHandleW(
                null(),
                windows_sys::w!("Kerberos"),
                SECPKG_CRED_OUTBOUND,
                null(),
                null(),
                None,
                null(),
                &mut credential,
                &mut expires,
            )
        };
        check(result, "Windows-Kerberos-Anmeldung")?;
        Ok(Self {
            credential,
            context: invalid_handle(),
        })
    }

    fn advance(
        &mut self,
        principal: &[u16],
        mut input: Option<Vec<u8>>,
        delegate: bool,
    ) -> io::Result<(bool, Vec<u8>)> {
        let mut input_buffer = input
            .as_deref_mut()
            .map(|bytes| buffer(SECBUFFER_TOKEN, bytes));
        let mut input = input_buffer
            .as_mut()
            .map(|item| descriptor(std::slice::from_mut(item)));
        let mut output = vec![0; MAX_TOKEN];
        let mut output_buffer = buffer(SECBUFFER_TOKEN, &mut output);
        let mut output_desc = descriptor(std::slice::from_mut(&mut output_buffer));
        let mut flags = 0;
        let mut expires = 0;
        let context = &raw mut self.context;
        let previous = if self.context.dwLower == usize::MAX {
            null()
        } else {
            context.cast_const()
        };
        // SAFETY: SSPI accepts the same initialized handle as input and output
        // on subsequent calls. All buffer descriptors outlive the call.
        let mut status = unsafe {
            InitializeSecurityContextW(
                &self.credential,
                previous,
                principal.as_ptr(),
                ISC_REQ_MUTUAL_AUTH
                    | ISC_REQ_INTEGRITY
                    | if delegate { ISC_REQ_DELEGATE } else { 0 },
                0,
                SECURITY_NATIVE_DREP,
                input.as_mut().map_or(null(), |input| &raw const *input),
                0,
                context,
                &mut output_desc,
                &mut flags,
                &mut expires,
            )
        };
        if matches!(status, SEC_I_COMPLETE_NEEDED | SEC_I_COMPLETE_AND_CONTINUE) {
            // SAFETY: SSPI returned a context and described the output token to complete.
            check(
                unsafe { CompleteAuthToken(context, &output_desc) },
                "Kerberos-Token",
            )?;
            status = if status == SEC_I_COMPLETE_NEEDED {
                SEC_E_OK
            } else {
                SEC_I_CONTINUE_NEEDED
            };
        }
        let produced = output_buffer.cbBuffer as usize;
        if produced > output.len() {
            return Err(io::Error::other(
                "Windows hat einen zu großen Kerberos-Token geliefert.",
            ));
        }
        output.truncate(produced);
        if status == SEC_I_CONTINUE_NEEDED && !output.is_empty() {
            return Ok((false, output));
        }
        check(status, "Kerberos-Dienstanmeldung")?;
        let required = ISC_RET_MUTUAL_AUTH | ISC_RET_INTEGRITY;
        if flags & required != required {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Die Kerberos-Verbindung hat keine gegenseitige Anmeldung mit Integritätsschutz bestätigt.",
            ));
        }
        if delegate && flags & ISC_RET_DELEGATE == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Windows hat keine Kerberos-Ticketdelegation für diesen SSH-Server bestätigt.",
            ));
        }
        Ok((true, output))
    }

    fn signature(&self, mut data: Vec<u8>) -> io::Result<Vec<u8>> {
        let mut sizes = SecPkgContext_Sizes::default();
        // SAFETY: The context was established and the attribute output has the expected type.
        check(
            unsafe {
                QueryContextAttributesW(&self.context, SECPKG_ATTR_SIZES, (&raw mut sizes).cast())
            },
            "Kerberos-Signaturgröße",
        )?;
        let length = sizes.cbMaxSignature as usize;
        if !(1..=MAX_TOKEN).contains(&length) {
            return Err(io::Error::other("Ungültige Kerberos-Signaturgröße."));
        }
        let mut signature = vec![0; length];
        let mut buffers = [
            buffer(SECBUFFER_DATA, &mut data),
            buffer(SECBUFFER_TOKEN, &mut signature),
        ];
        // SAFETY: Both buffers remain live and writable for the duration of the call.
        check(
            unsafe { MakeSignature(&self.context, 0, &descriptor(&mut buffers), 0) },
            "Kerberos-Signatur",
        )?;
        let written = buffers[1].cbBuffer as usize;
        if !(1..=length).contains(&written) {
            return Err(io::Error::other(
                "Windows hat keine gültige Kerberos-Signatur geliefert.",
            ));
        }
        signature.truncate(written);
        Ok(signature)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: The owned handles are released once, in dependency order.
        unsafe {
            if self.context.dwLower != usize::MAX {
                DeleteSecurityContext(&self.context);
            }
            FreeCredentialsHandle(&self.credential);
        }
    }
}

fn buffer(kind: u32, bytes: &mut [u8]) -> SecBuffer {
    SecBuffer {
        cbBuffer: bytes.len() as u32,
        BufferType: kind,
        pvBuffer: bytes.as_mut_ptr().cast(),
    }
}

fn descriptor(buffers: &mut [SecBuffer]) -> SecBufferDesc {
    SecBufferDesc {
        ulVersion: SECBUFFER_VERSION,
        cBuffers: buffers.len() as u32,
        pBuffers: buffers.as_mut_ptr(),
    }
}

fn invalid_handle() -> SecHandle {
    SecHandle {
        dwLower: usize::MAX,
        dwUpper: usize::MAX,
    }
}

fn check(status: i32, operation: &str) -> io::Result<()> {
    if status == SEC_E_OK {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{operation} fehlgeschlagen (SSPI 0x{:08X}).", status as u32),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::{Connection, Security};
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn gssapi_rejects_unoffered_mechanism_before_sspi() {
        let mut auth = Kerberos {
            principal: Vec::new(),
            context: None,
            complete: false,
            delegate: false,
        };
        let step = auth
            .gssapi_step(Some(vec![0x06, 1, 0x2a]), None, Vec::new())
            .await;
        assert_eq!(step.err().unwrap().0.kind(), io::ErrorKind::InvalidData);
        assert!(auth.context.is_none());
    }

    #[test]
    #[ignore = "requires an SSH server configured for Kerberos and a Windows Kerberos logon"]
    fn reference_kerberos_sftp() {
        let connection = Connection::new(
            &std::env::var("ARCO_TEST_KERBEROS_URL").unwrap(),
            &std::env::var("ARCO_TEST_KERBEROS_USER").unwrap(),
            Security::Sftp,
        )
        .unwrap();
        let fingerprint = std::env::var("ARCO_TEST_KERBEROS_FINGERPRINT").unwrap_or_default();
        let principal = std::env::var("ARCO_TEST_KERBEROS_SPN").unwrap_or_default();
        let connection = if std::env::var("ARCO_TEST_KERBEROS_KEX").as_deref() == Ok("1") {
            connection.with_ssh_kerberos_kex(&fingerprint, &principal)
        } else {
            connection
                .with_ssh(&fingerprint, None)
                .unwrap()
                .with_ssh_kerberos(&principal)
        };
        let session = connection
            .unwrap()
            .with_ssh_kerberos_delegation(
                std::env::var("ARCO_TEST_KERBEROS_DELEGATE").is_ok_and(|value| value == "1"),
            )
            .unwrap()
            .authenticate("")
            .unwrap();
        session
            .list(&session.initial_directory(), &AtomicBool::new(false))
            .unwrap();
    }
}
