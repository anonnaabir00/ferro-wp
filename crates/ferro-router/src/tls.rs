use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
};
use rustls::crypto::ring::sign::any_supported_type;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The Ferro local root CA. Generated once, kept in
/// ~/Library/Application Support/ferro/ca, and used to issue per-site
/// certificates on demand (SNI).
pub struct Ca {
    cert: Certificate,
    key: KeyPair,
    cert_pem: String,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl fmt::Debug for Ca {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ca").finish_non_exhaustive()
    }
}

impl Ca {
    pub fn load_or_create(dir: &Path) -> Result<Ca> {
        std::fs::create_dir_all(dir)?;
        let cert_path = dir.join("ferro-ca.pem");
        let key_path = dir.join("ferro-ca-key.pem");
        if cert_path.exists() && key_path.exists() {
            let cert_pem = std::fs::read_to_string(&cert_path)
                .with_context(|| format!("could not read {}", cert_path.display()))?;
            let key_pem = std::fs::read_to_string(&key_path)
                .with_context(|| format!("could not read {}", key_path.display()))?;
            let key = KeyPair::from_pem(&key_pem).context("CA key is not valid PEM")?;
            let params = CertificateParams::from_ca_cert_pem(&cert_pem)
                .context("CA cert is not valid PEM")?;
            let cert = params.self_signed(&key)?;
            return Ok(Ca {
                cert,
                key,
                cert_pem,
                cache: Mutex::new(HashMap::new()),
            });
        }

        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "Ferro WP Local CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "Ferro WP");
        let key = KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        let cert_pem = cert.pem();
        std::fs::write(&cert_path, &cert_pem)?;
        std::fs::write(&key_path, key.serialize_pem())?;
        set_private(&key_path)?;
        tracing::info!(path = %cert_path.display(), "generated local CA");
        Ok(Ca {
            cert,
            key,
            cert_pem,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Issue (or return cached) a leaf certificate for `host`.
    pub fn issue(&self, host: &str) -> Result<Arc<CertifiedKey>> {
        if let Some(key) = self.cache.lock().unwrap().get(host) {
            return Ok(key.clone());
        }
        let params = CertificateParams::new(vec![host.to_string()])?;
        let key_pair = KeyPair::generate()?;
        let cert = params.signed_by(&key_pair, &self.cert, &self.key)?;
        let cert_der: CertificateDer<'static> = cert.der().clone();
        let key_der = PrivatePkcs8KeyDer::from(key_pair.serialize_der());
        let signing = any_supported_type(&PrivateKeyDer::Pkcs8(key_der))
            .context("could not build TLS signing key")?;
        let certified = Arc::new(CertifiedKey::new(vec![cert_der], signing));
        self.cache
            .lock()
            .unwrap()
            .insert(host.to_string(), certified.clone());
        Ok(certified)
    }
}

impl rustls::server::ResolvesServerCert for Ca {
    fn resolve(&self, client_hello: rustls::server::ClientHello) -> Option<Arc<CertifiedKey>> {
        let host = client_hello.server_name()?.to_string();
        match self.issue(&host) {
            Ok(key) => Some(key),
            Err(err) => {
                tracing::warn!("could not issue cert for {host}: {err:#}");
                None
            }
        }
    }
}

fn set_private(path: &PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
