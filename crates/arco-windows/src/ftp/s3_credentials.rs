use super::{
    Connection, Security, Session, check_cancel, check_password, http::HttpRequest, invalid_input,
};
use aws_config::{
    BehaviorVersion, Region, default_provider::credentials::DefaultCredentialsChain,
    profile::ProfileFileCredentialsProvider, provider_config::ProviderConfig,
    sts::AssumeRoleProvider,
};
use aws_credential_types::{Credentials, provider::ProvideCredentials};
use aws_smithy_http_client::{
    Builder,
    tls::{Provider, rustls_provider::CryptoMode},
};
use std::io;
use std::sync::{Mutex, OnceLock, TryLockError, atomic::AtomicBool};
use std::time::{Duration, Instant, SystemTime};
use tokio::runtime::Runtime;
use zeroize::Zeroizing;

const REFRESH_MAX: Duration = Duration::from_secs(600);
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
pub(super) const MAX_SESSION_TOKEN: usize = 16 * 1024;

pub(super) struct CachedCredentials {
    value: Credentials,
    refresh: Instant,
}

impl Connection {
    pub fn with_s3_role_arn(mut self, arn: &str) -> io::Result<Self> {
        let arn = arn.trim();
        if arn.is_empty() {
            return Ok(self);
        }
        if self.security != Security::S3 || !self.secure_s3_endpoint() {
            return Err(invalid_input("Die S3-Rollenanmeldung benötigt HTTPS."));
        }
        let mut parts = arn.splitn(6, ':');
        let valid = matches!(
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
            (Some("arn"), Some(partition), Some("iam"), Some(""), Some(account), Some(role))
                if !partition.is_empty()
                && partition.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && account.len() == 12 && account.bytes().all(|byte| byte.is_ascii_digit())
                && role.strip_prefix("role/").is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_graphic()))
        );
        if arn.len() > 2048 || !valid {
            return Err(invalid_input("Gib eine gültige IAM-Rollen-ARN ein."));
        }
        self.s3_role_arn = Some(arn.to_owned());
        Ok(self)
    }

    pub fn with_s3_session_token(mut self, token: Zeroizing<String>) -> io::Result<Self> {
        self.check_s3_session_token(&token)?;
        self.s3_session_token = Some(token);
        Ok(self)
    }

    pub fn has_s3_session_token(&self) -> bool {
        self.s3_session_token.is_some()
    }

    pub fn with_s3_temporary_credentials(mut self, required: bool) -> io::Result<Self> {
        if required {
            self.validate_s3_token_context()?;
        }
        self.s3_token_required = required;
        Ok(self)
    }

    fn validate_s3_token_context(&self) -> io::Result<()> {
        if self.security != Security::S3
            || !self.secure_s3_endpoint()
            || self.s3_aws_profile.is_some()
            || self.user.is_empty()
        {
            return Err(invalid_input(
                "Ein direktes S3-Sitzungstoken benötigt eine Zugriffsschlüssel-ID und HTTPS.",
            ));
        }
        Ok(())
    }

    pub fn check_s3_session_token(&self, token: &str) -> io::Result<()> {
        self.validate_s3_token_context()?;
        if token.is_empty()
            || token.len() > MAX_SESSION_TOKEN
            || !token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(invalid_input("Ungültiges S3-Sitzungstoken."));
        }
        Ok(())
    }

    fn secure_s3_endpoint(&self) -> bool {
        self.url.scheme() == "https"
            || matches!(self.url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(self.url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
            || self.url.host_str() == Some("localhost")
    }

    pub fn with_s3_aws_profile(mut self, profile: Option<&str>) -> io::Result<Self> {
        if let Some(name) = profile {
            if self.security != Security::S3 || !self.user.is_empty() {
                return Err(invalid_input(
                    "Die AWS-Anmeldekette benötigt ein S3-Profil ohne Zugriffsschlüssel-ID.",
                ));
            }
            if name.len() > 128 || name.chars().any(char::is_control) {
                return Err(invalid_input("Ungültiger AWS-Profilname."));
            }
            if !self.secure_s3_endpoint() {
                return Err(invalid_input("AWS-Profilanmeldung benötigt HTTPS."));
            }
            self.s3_aws_profile = Some(name.trim().to_owned());
        }
        Ok(self)
    }
}

impl Session {
    pub(super) fn sign_s3_request(
        &self,
        request: &mut HttpRequest<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.connection.s3_aws_profile.is_some() || self.connection.s3_role_arn.is_some() {
            let credentials = self.aws_credentials(cancel)?;
            request.credentials(credentials.access_key_id(), credentials.secret_access_key());
            if let Some(token) = credentials.session_token() {
                request.header("x-amz-security-token", token)?;
            }
        } else {
            request.credentials(&self.connection.user, &self.password);
            if let Some(token) = &self.s3_token {
                request.header("x-amz-security-token", token)?;
            }
        }
        Ok(())
    }

    fn aws_credentials(&self, cancel: &AtomicBool) -> io::Result<Credentials> {
        let mut cache = lock_cache(&self.aws_credentials, cancel)?;
        if let Some(cached) = &*cache
            && Instant::now() < cached.refresh
            && !cached
                .value
                .expiry()
                .is_some_and(|time| time <= SystemTime::now())
        {
            return Ok(cached.value.clone());
        }
        let credentials = runtime()?.block_on(async {
            let fetch = async {
                let http = Builder::new()
                    .tls_provider(Provider::Rustls(CryptoMode::Ring))
                    .build_https();
                let region = Region::new(self.connection.s3_region.clone());
                let config = ProviderConfig::without_region()
                    .with_region(Some(region.clone()))
                    .with_http_client(http.clone());
                let base = match self.connection.s3_aws_profile.as_deref() {
                    Some("") => DefaultCredentialsChain::builder().configure(config)
                        .build().await.provide_credentials().await?,
                    Some(profile) => ProfileFileCredentialsProvider::builder()
                        .configure(&config).profile_name(profile).build().provide_credentials().await?,
                    None => Credentials::new(
                        self.connection.user.clone(),
                        self.password.as_str(),
                        self.s3_token.as_ref().map(|token| token.as_str().to_owned()),
                        None,
                        "arco-s3",
                    ),
                };
                let Some(role) = self.connection.s3_role_arn.as_deref() else {
                    return Ok(base);
                };
                let sdk = aws_config::defaults(BehaviorVersion::latest()).region(region)
                    .http_client(http).credentials_provider(base).load().await;
                AssumeRoleProvider::builder(role).configure(&sdk).session_name("arco-commander")
                    .build().await.provide_credentials().await
            };
            let fetch = tokio::time::timeout(SETUP_TIMEOUT, fetch);
            tokio::pin!(fetch);
            loop {
                check_cancel(cancel)?;
                tokio::select! {
                    result = &mut fetch => return result
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Die AWS-Anmeldung hat das Zeitlimit erreicht."))?
                        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, if self.connection.s3_role_arn.is_some() {
                            "Die AWS-Rollenanmeldung ist fehlgeschlagen. Prüfe die Quellzugangsdaten, Rollen-ARN und STS-Berechtigungen."
                        } else {
                            "Die AWS-Anmeldekette hat keine gültigen Zugangsdaten geliefert. Prüfe das AWS-Profil und seine aktuelle Anmeldung."
                        })),
                    _ = tokio::time::sleep(POLL) => (),
                }
            }
        })?;
        let key = credentials.access_key_id();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || credentials.secret_access_key().is_empty()
        {
            return Err(invalid_input(
                "Die AWS-Anmeldekette hat ungültige Zugriffsschlüssel geliefert.",
            ));
        }
        check_password(credentials.secret_access_key())?;
        let refresh = if let Some(expiry) = credentials.expiry() {
            let remaining = expiry
                .duration_since(SystemTime::now())
                .ok()
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Die AWS-Anmeldung ist abgelaufen.",
                    )
                })?;
            remaining
                .saturating_sub(Duration::from_secs(60))
                .min(REFRESH_MAX)
        } else {
            REFRESH_MAX
        };
        *cache = Some(CachedCredentials {
            refresh: Instant::now() + refresh,
            value: credentials.clone(),
        });
        Ok(credentials)
    }
}

fn runtime() -> io::Result<&'static Runtime> {
    static RUNTIME: OnceLock<io::Result<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("arco-aws")
                .enable_all()
                .build()
        })
        .as_ref()
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
}

fn lock_cache<'a>(
    cache: &'a Mutex<Option<CachedCredentials>>,
    cancel: &AtomicBool,
) -> io::Result<std::sync::MutexGuard<'a, Option<CachedCredentials>>> {
    loop {
        check_cancel(cancel)?;
        match cache.try_lock() {
            Ok(cache) => return Ok(cache),
            Err(TryLockError::WouldBlock) => std::thread::sleep(POLL),
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("Der AWS-Anmeldestatus ist beschädigt."));
            }
        }
    }
}
