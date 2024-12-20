use anyhow::Error;
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, KeyIdMethod, SanType, PKCS_ECDSA_P256_SHA256};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use time::{ext::NumericalDuration, OffsetDateTime};

use std::path::PathBuf;

use crate::transport::{ServerCertHash, WebServerDestination};

/// Generates a self-signed certificate for use in `WebTransportConfig`.
///
/// The [`PrivateKey`] should not be publicized.
pub fn generate_self_signed_certificate(
    params: CertificateParams,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), rcgen::Error> {
    let keypair = rcgen::KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let rc_certificate = params.self_signed(&keypair)?;
    let certificate = rc_certificate.der().clone();
    let privkey = PrivateKeyDer::Pkcs8(keypair.serialize_der().into());

    Ok((certificate, privkey))
}

/// Generates a self-signed certificate for use in `WebTransportConfig`.
///
/// The certificate generated is opinionated based on how it is expected to be used.
/// - The validity period is set to 2 weeks from now to support [`ServerCertHash`].
/// - ECDSA is used, not RSA.
///
/// The [`PrivateKey`] should not be publicized.
pub fn generate_self_signed_certificate_opinionated<T: Into<WebServerDestination>>(
    subject_alt_names: impl IntoIterator<Item = T>,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), rcgen::Error> {
    let not_before = OffsetDateTime::now_utc().saturating_sub(1.hours()); //adjust for client system time variance
    let not_after = not_before.saturating_add(2.weeks());
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, "renet2 self signed cert");

    let mut subject_alt_names_collected: Vec<SanType> = vec![];
    for dest in subject_alt_names.into_iter() {
        let san_type = match dest.into() {
            WebServerDestination::Addr(addr) => SanType::IpAddress(addr.ip()),
            WebServerDestination::Url(url) => match url.domain() {
                Some(domain) => SanType::DnsName(domain.try_into()?),
                None => SanType::URI(String::from(url).try_into()?),
            },
        };
        subject_alt_names_collected.push(san_type);
    }

    let mut params = CertificateParams::default(); //the params is `non_exhaustive` so we need to default construct
    params.not_before = not_before;
    params.not_after = not_after;
    params.serial_number = None;
    params.subject_alt_names = subject_alt_names_collected;
    params.distinguished_name = distinguished_name;
    params.is_ca = IsCa::NoCa;
    params.key_usages = Vec::new();
    params.extended_key_usages = Vec::new();
    params.name_constraints = None;
    params.crl_distribution_points = Vec::new();
    params.custom_extensions = Vec::new();
    params.use_authority_key_identifier_extension = false;
    params.key_identifier_method = KeyIdMethod::Sha256;

    generate_self_signed_certificate(params)
}

/// Loads a certificate and private key from the file system.
///
/// The certificate and key must be DER encoded.
pub fn get_certificate_and_key_from_files(cert: PathBuf, key: PathBuf) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), Error> {
    let cert = CertificateDer::from(std::fs::read(cert)?);
    let key = PrivateKeyDer::Pkcs8(std::fs::read(key)?.into());
    Ok((cert, key))
}

/*
/// SPKI fingerprint is needed when launching a Chrome browser with a custom cert chain.
fn _spki_fingerprint(cert: &Certificate) -> Option<spki::FingerprintBytes> {
    let cert = x509_cert::Certificate::from_der(&cert.0).ok()?;
    let fingerprint = cert
        .tbs_certificate
        .subject_public_key_info
        .fingerprint_bytes()
        .ok()?;
    Some(fingerprint)
}

fn _spki_fingerprint_base64(cert: &Certificate) -> Option<String> {
    _spki_fingerprint(cert)
        .map(|fingerprint| base64::engine::general_purpose::STANDARD.encode(fingerprint))
}
*/

/// Gets a [`ServerCertHash`] from a [`Certificate`].
pub fn get_server_cert_hash(cert: &CertificateDer<'_>) -> ServerCertHash {
    let hash = hmac_sha256::Hash::hash(cert.as_ref());
    ServerCertHash { hash }
}
