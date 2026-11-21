use arco_core::profile::{
    FtpAddressFamily, FtpListing, FtpProfile, FtpSecurity, FtpTransferMode, SshAgent,
};
use arco_windows::ftp::{self, Connection};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use zeroize::Zeroizing;

pub(crate) fn connection(profile: &FtpProfile) -> io::Result<Connection> {
    let mut connection = connection_settings(profile)?;
    if let Some(id) = profile.proxy_credential.as_deref() {
        connection = connection.use_proxy_password(id)?;
    }
    if profile.security == FtpSecurity::S3
        && profile.s3_temporary
        && let Some(id) = profile.s3_token_credential.as_deref()
    {
        connection = connection.use_s3_session_token(id)?;
    }
    Ok(connection)
}

pub(crate) fn connection_settings(profile: &FtpProfile) -> io::Result<Connection> {
    let mode = match profile.security {
        FtpSecurity::ExplicitTls => ftp::Security::ExplicitTls,
        FtpSecurity::ImplicitTls => ftp::Security::ImplicitTls,
        FtpSecurity::Plain => ftp::Security::Plain,
        FtpSecurity::Sftp => ftp::Security::Sftp,
        FtpSecurity::Scp => ftp::Security::Scp,
        FtpSecurity::WebDav => ftp::Security::WebDav,
        FtpSecurity::S3 => ftp::Security::S3,
    };
    let listing = match profile.listing {
        FtpListing::Automatic => ftp::ListingMode::Automatic,
        FtpListing::Mlsd => ftp::ListingMode::Mlsd,
        FtpListing::List => ftp::ListingMode::List,
    };
    if profile.transfer.masks.len() > 4096 {
        return Err(invalid("Die Transfermaske ist zu lang."));
    }
    arco_core::sync::FileMasks::new(&profile.transfer.masks)?;
    let connection = Connection::new(&profile.address, &profile.user, mode)?
        .with_remote_recycle_bin(profile.recycle_remote)?
        .with_recycled_overwrites(profile.recycle_overwrites)?
        .with_proxy(&profile.proxy)?
        .with_telnet_proxy_command(&profile.telnet_proxy_command)?
        .with_local_proxy_command(&profile.local_proxy_command)?
        .with_ssh_proxy_dns_locally(profile.ssh_proxy_dns_locally)?
        .with_ftp_gateway(profile.ftp_gateway)?
        .with_proxy_user(&profile.proxy_user)?
        .with_listing_mode(listing)
        .with_active_mode(profile.active)?
        .with_transfer_settings(ftp::TransferSettings {
            preset: profile.transfer_preset.clone(),
            mode: match profile.transfer.mode {
                FtpTransferMode::Binary => ftp::PreferredTransferMode::Binary,
                FtpTransferMode::Text => ftp::PreferredTransferMode::Text,
                FtpTransferMode::Automatic => ftp::PreferredTransferMode::Automatic,
            },
            speed_kib: profile.transfer.speed_kib,
            preserve_time: profile.transfer.preserve_time,
            masks: profile.transfer.masks.clone(),
            names: profile.transfer.names,
        })?
        .with_options(ftp::ConnectionOptions {
            family: match profile.address_family {
                FtpAddressFamily::Automatic => ftp::AddressFamily::Automatic,
                FtpAddressFamily::Ipv4 => ftp::AddressFamily::Ipv4,
                FtpAddressFamily::Ipv6 => ftp::AddressFamily::Ipv6,
            },
            charset: profile.charset.clone(),
            list_pattern: profile.list_pattern.clone(),
            show_hidden: profile.show_hidden,
            ca_bundle: profile.ca_bundle.clone(),
            client_certificate: profile.client_certificate.clone(),
            mode_z: profile.mode_z,
            http_forward: profile.http_forward,
            otp: profile.otp,
        })?;
    if mode == ftp::Security::S3 {
        return connection
            .with_s3_region(&profile.s3_region)?
            .with_s3_request_options(profile.s3_virtual_host, profile.s3_requester_pays)?
            .with_s3_aws_profile(profile.s3_aws_profile.as_deref())?
            .with_s3_role_arn(&profile.s3_role_arn)?
            .with_s3_temporary_credentials(profile.s3_temporary);
    }
    if mode == ftp::Security::WebDav {
        return connection
            .with_webdav_options(profile.webdav_liberal_escaping, profile.webdav_windows_auth)?
            .with_webdav_redirects(
                profile.webdav_cross_domain_redirects,
                profile.webdav_unencrypted_redirects,
            );
    }
    if mode.is_ssh() {
        let connection = if profile.ssh_kerberos_kex {
            let principal = profile.ssh_kerberos.as_deref().ok_or_else(|| {
                invalid("Wähle Windows-Kerberos für den GSSAPI-Schlüsselaustausch.")
            })?;
            if profile.ssh_private_key.is_some() || profile.ssh_agent.is_some() {
                return Err(invalid(
                    "Der GSSAPI-Schlüsselaustausch benötigt ein eigenes Kerberos-Profil.",
                ));
            }
            connection.with_ssh_kerberos_kex(&profile.ssh_fingerprint, principal)?
        } else {
            connection.with_ssh(&profile.ssh_fingerprint, profile.ssh_private_key.clone())?
        };
        let connection = connection
            .with_ssh_timing(profile.ssh_timeout, profile.ssh_keepalive)?
            .with_ssh_interactive(profile.ssh_interactive)?;
        let connection = match profile.ssh_agent {
            Some(SshAgent::OpenSsh) => connection.with_ssh_agent(ftp::SshAgent::OpenSsh),
            Some(SshAgent::Pageant) => connection.with_ssh_agent(ftp::SshAgent::Pageant),
            None => Ok(connection),
        }?;
        let connection = if let Some(principal) = profile
            .ssh_kerberos
            .as_deref()
            .filter(|_| !profile.ssh_kerberos_kex)
        {
            connection.with_ssh_kerberos(principal)?
        } else {
            connection
        };
        connection.with_ssh_kerberos_delegation(profile.ssh_kerberos_delegate)
    } else {
        Ok(connection)
    }
}

pub(crate) fn jump_profiles<'a>(
    name: &'a str,
    profile: &'a FtpProfile,
    profiles: &'a BTreeMap<String, FtpProfile>,
) -> io::Result<Vec<(&'a str, &'a FtpProfile)>> {
    let mut visited = BTreeSet::from([name]);
    let mut chain = Vec::new();
    let mut next = profile.ssh_jump.as_deref();
    while let Some(name) = next {
        if !visited.insert(name) {
            return Err(invalid("Die SSH-Sprungserverkette enthält einen Kreis."));
        }
        if chain.len() >= ftp::MAX_SSH_JUMPS {
            return Err(invalid(
                "Die SSH-Sprungserverkette darf höchstens acht Server enthalten.",
            ));
        }
        let profile = profiles
            .get(name)
            .filter(|profile| profile.security.is_ssh())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("SFTP-Sprungprofil „{name}“ fehlt."),
                )
            })?;
        chain.push((name, profile));
        next = profile.ssh_jump.as_deref();
    }
    if !chain.is_empty() && !profile.proxy.is_empty() {
        return Err(invalid(
            "Trage den Proxy im Profil des ersten SSH-Sprungservers ein.",
        ));
    }
    chain.reverse();
    Ok(chain)
}

pub(crate) fn saved_secret(
    profile: &FtpProfile,
    connection: &Connection,
) -> io::Result<Option<Zeroizing<String>>> {
    if profile.ssh_agent.is_some()
        || profile.ssh_kerberos.is_some()
        || (profile.security == FtpSecurity::WebDav && profile.webdav_windows_auth)
        || (matches!(profile.security, FtpSecurity::WebDav | FtpSecurity::S3)
            && profile.user.is_empty())
    {
        return Ok(Some(Zeroizing::default()));
    }
    let secret = profile
        .credential
        .as_deref()
        .map(|id| connection.read_password(id))
        .transpose()
        .map(Option::flatten)?;
    if secret.is_none() && profile.ssh_interactive && profile.ssh_private_key.is_none() {
        return Ok(Some(Zeroizing::default()));
    }
    Ok(secret)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
