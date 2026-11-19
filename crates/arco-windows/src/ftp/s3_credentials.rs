use super::{
    Connection, Security, Session, check_cancel, check_password, http::HttpRequest, invalid_input,
};
use aws_config::{
    Region, default_provider::credentials::DefaultCredentialsChain,
    profile::ProfileFileCredentialsProvider, provider_config::ProviderConfig,
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

const REFRESH_MAX: Duration = Duration::from_secs(600);
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

pub(super) struct CachedCredentials {
    value: Credentials,
    refresh: Instant,
}

impl Connection {
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
            if self.url.scheme() != "https"
                && !matches!(self.url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
                && !matches!(self.url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
                && self.url.host_str() != Some("localhost")
            {
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
        if let Some(profile) = self.connection.s3_aws_profile.as_deref() {
            let credentials = self.aws_credentials(profile, cancel)?;
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

    fn aws_credentials(&self, profile: &str, cancel: &AtomicBool) -> io::Result<Credentials> {
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
                let config = ProviderConfig::without_region()
                    .with_region(Some(Region::new(self.connection.s3_region.clone())))
                    .with_http_client(http);
                if profile.is_empty() {
                    DefaultCredentialsChain::builder()
                        .configure(config)
                        .build().await
                        .provide_credentials().await
                } else {
                    ProfileFileCredentialsProvider::builder()
                        .configure(&config)
                        .profile_name(profile)
                        .build()
                        .provide_credentials().await
                }
            };
            let fetch = tokio::time::timeout(SETUP_TIMEOUT, fetch);
            tokio::pin!(fetch);
            loop {
                check_cancel(cancel)?;
                tokio::select! {
                    result = &mut fetch => return result
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Die AWS-Anmeldung hat das Zeitlimit erreicht."))?
                        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "Die AWS-Anmeldekette hat keine gültigen Zugangsdaten geliefert. Prüfe das AWS-Profil und seine aktuelle Anmeldung.")),
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
