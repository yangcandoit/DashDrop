use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::SigningKey;
use pkcs8::EncodePrivateKey;
use rand::rngs::OsRng;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

const KEYCHAIN_SERVICE: &str = "com.dashdrop.identity";

/// Device long-term identity backed by an Ed25519 keypair and self-signed X.509 cert.
#[derive(Clone)]
pub struct Identity {
    /// Base64-encoded SHA256 of the DER-encoded certificate.
    pub fingerprint: String,
    /// DER-encoded self-signed TLS certificate (valid 10 years).
    pub cert_der: Vec<u8>,
    /// DER-encoded private key (PKCS#8).
    pub key_der: Vec<u8>,
    /// Human-readable display name for this device.
    pub device_name: String,
}

impl Identity {
    /// Load existing identity from disk, or generate a fresh one.
    pub fn load_or_create(config_dir: &PathBuf) -> Result<Self> {
        fs::create_dir_all(config_dir).context("create config dir")?;

        let key_path = config_dir.join("identity.key");
        let cert_path = config_dir.join("identity.cert");
        let secure_account = format!("config:{}", config_dir.display());
        let secure_store = crate::crypto::secret_store::secure_store_available();

        let (key_der, cert_der) = if cert_path.exists() {
            let cd = fs::read(&cert_path).context("read identity cert")?;
            let key_der = if secure_store {
                if let Some(kd) =
                    crate::crypto::secret_store::load_private_key(KEYCHAIN_SERVICE, &secure_account)?
                {
                    kd
                } else if key_path.exists() {
                    // One-time migration from legacy plaintext key file.
                    let kd = fs::read(&key_path).context("read legacy identity key")?;
                    crate::crypto::secret_store::save_private_key(
                        KEYCHAIN_SERVICE,
                        &secure_account,
                        &kd,
                    )
                    .context("migrate key into secure store")?;
                    let _ = fs::remove_file(&key_path);
                    kd
                } else {
                    tracing::warn!("Certificate exists but private key missing; regenerating identity");
                    let (new_kd, new_cd) = Self::generate()?;
                    crate::crypto::secret_store::save_private_key(
                        KEYCHAIN_SERVICE,
                        &secure_account,
                        &new_kd,
                    )
                    .context("save regenerated key into secure store")?;
                    fs::write(&cert_path, &new_cd).context("write regenerated identity cert")?;
                    return Self::from_parts(new_kd, new_cd);
                }
            } else {
                fs::read(&key_path).context("read identity key")?
            };
            (key_der, cd)
        } else {
            tracing::info!("Generating new device identity...");
            let (kd, cd) = Self::generate()?;
            if secure_store {
                crate::crypto::secret_store::save_private_key(KEYCHAIN_SERVICE, &secure_account, &kd)
                    .context("write identity key to secure store")?;
            } else {
                fs::write(&key_path, &kd).context("write identity key")?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = fs::metadata(&key_path) {
                        let mut perms = metadata.permissions();
                        perms.set_mode(0o600);
                        let _ = fs::set_permissions(&key_path, perms);
                    }
                }
            }

            fs::write(&cert_path, &cd).context("write identity cert")?;
            (kd, cd)
        };

        if secure_store && key_path.exists() {
            // Legacy cleanup: never keep plaintext key once secure storage is in use.
            let _ = fs::remove_file(&key_path);
        }

        Self::from_parts(key_der, cert_der)
    }

    fn from_parts(key_der: Vec<u8>, cert_der: Vec<u8>) -> Result<Self> {
        let fingerprint = Self::compute_fingerprint(&cert_der);
        let device_name = get_hostname();

        Ok(Identity {
            fingerprint,
            cert_der,
            key_der,
            device_name,
        })
    }

    /// Generate a new Ed25519 keypair, sign a self-signed X.509 cert.
    fn generate() -> Result<(Vec<u8>, Vec<u8>)> {
        // Generate Ed25519 signing key
        let signing_key = SigningKey::generate(&mut OsRng);

        // Export PKCS#8 DER
        let key_der = signing_key
            .to_pkcs8_der()
            .context("encode key to PKCS#8")?
            .as_bytes()
            .to_vec();

        // Build self-signed X.509 cert with rcgen using the PEM key
        let key_pem = signing_key
            .to_pkcs8_pem(Default::default())
            .context("signing key to PEM")?
            .to_string();

        let key_pair = KeyPair::from_pem(&key_pem).context("rcgen KeyPair from PEM")?;

        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "DashDrop Device");
        params.distinguished_name = dn;
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2034, 1, 1);

        let cert = params.self_signed(&key_pair).context("self-sign cert")?;
        let cert_der = cert.der().to_vec();

        Ok((key_der, cert_der))
    }

    /// Compute fingerprint = base64(SHA256(cert DER bytes)).
    pub fn compute_fingerprint(cert_der: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(cert_der);
        BASE64.encode(hasher.finalize())
    }

    /// Compute the fingerprint of a peer certificate received during TLS.
    pub fn peer_fingerprint(cert_der: &[u8]) -> String {
        Self::compute_fingerprint(cert_der)
    }

    /// Build a rustls-based ServerConfig for use with quinn.
    pub fn server_tls_config(&self) -> Result<std::sync::Arc<rustls::ServerConfig>> {
        use rustls::ServerConfig;

        let cert = rustls_pki_types::CertificateDer::from(self.cert_der.clone());
        let key = rustls_pki_types::PrivateKeyDer::try_from(self.key_der.clone())
            .map_err(|e| anyhow::anyhow!("invalid private key: {e}"))?;

        let cfg = ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS version")?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("server cert")?;

        Ok(std::sync::Arc::new(cfg))
    }

    /// Build a rustls ClientConfig that accepts any self-signed cert (TOFU).
    pub fn client_tls_config(&self) -> Result<std::sync::Arc<rustls::ClientConfig>> {
        use crate::crypto::cert_verifier::SkipServerVerification;
        use rustls::ClientConfig;

        let cfg = ClientConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS version")?
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();

        Ok(std::sync::Arc::new(cfg))
    }
}

fn get_hostname() -> String {
    // Use std::process to call hostname command as fallback, or env var
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "DashDrop-Device".to_string())
        })
}
