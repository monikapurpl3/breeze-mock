//! TLS for mock servers: a certificate authority per blueprint, certificates
//! it issues for each server's domains, and the choice between them by SNI.
//!
//! The authority is kept on disk (`data::for_blueprint(name)/ca.pem`), so a
//! client told to trust it once -- `breeze-mock export ca` -- keeps trusting
//! every later run. Server certificates are issued fresh at each start.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

pub struct Ca {
    /// As written to disk and handed to clients.
    pub pem: String,
    key: KeyPair,
    /// The same authority rebuilt for signing: same name and key as the one
    /// on disk, which is all a certificate it signs refers to.
    cert: rcgen::Certificate,
}

fn ca_params(blueprint: &str) -> Result<CertificateParams, String> {
    let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("breeze-mock CA ({blueprint})"));
    dn.push(DnType::OrganizationName, "breeze-mock");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    p.not_before = rcgen::date_time_ymd(2026, 1, 1);
    p.not_after = rcgen::date_time_ymd(2046, 1, 1);
    Ok(p)
}

impl Ca {
    /// The blueprint's authority, made on first use.
    pub fn load_or_create(dir: &Path, blueprint: &str) -> Result<Self, String> {
        let cert_path = dir.join("ca.pem");
        let key_path = dir.join("ca.key");
        let params = ca_params(blueprint)?;
        if cert_path.exists() && key_path.exists() {
            let pem = std::fs::read_to_string(&cert_path)
                .map_err(|e| format!("cannot read {}: {e}", cert_path.display()))?;
            let key_pem = std::fs::read_to_string(&key_path)
                .map_err(|e| format!("cannot read {}: {e}", key_path.display()))?;
            let key = KeyPair::from_pem(&key_pem)
                .map_err(|e| format!("{} is not a usable key: {e}", key_path.display()))?;
            let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
            return Ok(Self { pem, key, cert });
        }
        crate::data::private_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        let pem = cert.pem();
        crate::data::private_file(&key_path, key.serialize_pem().as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", key_path.display()))?;
        std::fs::write(&cert_path, &pem).map_err(|e| format!("cannot write {}: {e}", cert_path.display()))?;
        crate::info!("tls", "made a certificate authority for this blueprint: {}", cert_path.display());
        Ok(Self { pem, key, cert })
    }

    /// A server certificate for these names and addresses.
    pub fn issue(&self, names: &[String], ips: &[IpAddr]) -> Result<Arc<CertifiedKey>, String> {
        let mut sans: Vec<String> = names.to_vec();
        sans.extend(ips.iter().map(|ip| ip.to_string()));
        let mut p = CertificateParams::new(sans).map_err(|e| e.to_string())?;
        let mut dn = DistinguishedName::new();
        dn.push(
            DnType::CommonName,
            names.first().cloned().unwrap_or_else(|| "breeze-mock".into()),
        );
        p.distinguished_name = dn;
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        // Strict verifiers -- OpenSSL 3 with X509_STRICT, which Python 3.13
        // turns on by default -- refuse a certificate without it.
        p.use_authority_key_identifier_extension = true;
        let now = chrono::Utc::now();
        use chrono::Datelike;
        p.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
        let later = now + chrono::Duration::days(397);
        p.not_after = rcgen::date_time_ymd(later.year(), later.month() as u8, later.day() as u8);
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = p
            .signed_by(&key, &self.cert, &self.key)
            .map_err(|e| e.to_string())?;
        let chain = vec![cert.der().clone()];
        certified(chain, PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())))
    }
}

fn certified(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<Arc<CertifiedKey>, String> {
    let signer = rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| e.to_string())?;
    Ok(Arc::new(CertifiedKey::new(chain, signer)))
}

/// A certificate and key of the user's own, from PEM files.
pub fn from_files(cert: &Path, key: &Path) -> Result<Arc<CertifiedKey>, String> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("cannot read {}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert.display()))?;
    if chain.is_empty() {
        return Err(format!("{} holds no certificate", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("cannot read {}: {e}", key.display()))?;
    certified(chain, key)
}

/// Picks a certificate by the name the client asked for.
#[derive(Debug)]
pub struct Resolver {
    pub by_name: HashMap<String, Arc<CertifiedKey>>,
    /// For a client that sent no name, or one nobody has -- connecting by
    /// address sends none.
    pub fallback: Option<Arc<CertifiedKey>>,
}

impl ResolvesServerCert for Resolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        hello
            .server_name()
            .and_then(|n| self.by_name.get(&n.to_ascii_lowercase()))
            .cloned()
            .or_else(|| self.fallback.clone())
    }
}

pub fn server_config(resolver: Resolver) -> Result<Arc<rustls::ServerConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver));
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
