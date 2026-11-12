//! Certificate encoding by rcgen with installed platform RSA signing.
#![forbid(unsafe_code)]
pub use rcgen::*;

pub struct RsaSigningKey {
    signer: platform_tls::native_tls::RsaSigningKey,
    public_der: Vec<u8>,
    private_pem: zeroize::Zeroizing<String>,
}
impl RsaSigningKey {
    pub fn serialize_pem(&self) -> String {
        self.private_pem.to_string()
    }
}
impl PublicKeyData for RsaSigningKey {
    fn der_bytes(&self) -> &[u8] {
        &self.public_der
    }
    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &PKCS_RSA_SHA256
    }
}
impl SigningKey for RsaSigningKey {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        self.signer
            .sign_sha256(message)
            .map_err(|_| Error::RemoteKeyError)
    }
}
pub fn generate_simple_self_signed(
    names: impl Into<Vec<String>>,
) -> Result<CertifiedKey<RsaSigningKey>, Error> {
    let mut parameters = CertificateParams::new(names)?;
    let signer = platform_tls::native_tls::RsaSigningKey::generate()
        .map_err(|_| Error::KeyGenerationUnavailable)?;
    let public_der = signer
        .public_key_der()
        .map_err(|_| Error::CouldNotParseKeyPair)?;
    let private_pem = signer
        .private_key_pem()
        .map_err(|_| Error::CouldNotParseKeyPair)?;
    let signing_key = RsaSigningKey {
        signer,
        public_der,
        private_pem,
    };
    let mut serial = [0u8; 20];
    getrandom::fill(&mut serial).map_err(|_| Error::KeyGenerationUnavailable)?;
    serial[0] &= 0x7f;
    serial[0] |= 1;
    parameters.serial_number = Some(SerialNumber::from_slice(&serial));
    let cert = parameters.self_signed(&signing_key)?;
    Ok(CertifiedKey { cert, signing_key })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CachedCert {
        key_pem: String,
        key_der: Vec<u8>,
        cert_pem: String,
        cert_der: Vec<u8>,
    }

    fn cached_localhost_cert() -> Result<&'static CachedCert, Error> {
        static CACHED: std::sync::OnceLock<Option<CachedCert>> = std::sync::OnceLock::new();
        CACHED
            .get_or_init(|| {
                let CertifiedKey { cert, signing_key } =
                    generate_simple_self_signed(vec!["localhost".to_string()]).ok()?;
                Some(CachedCert {
                    key_pem: signing_key.serialize_pem(),
                    key_der: signing_key.der_bytes().to_vec(),
                    cert_pem: cert.pem(),
                    cert_der: cert.der().to_vec(),
                })
            })
            .as_ref()
            .ok_or(Error::KeyGenerationUnavailable)
    }

    fn cached_signing_key() -> Result<RsaSigningKey, Error> {
        let cached = cached_localhost_cert()?;
        let signer = platform_tls::native_tls::RsaSigningKey::from_pkcs8(cached.key_pem.as_bytes())
            .map_err(|_| Error::CouldNotParseKeyPair)?;
        let private_pem = signer
            .private_key_pem()
            .map_err(|_| Error::CouldNotParseKeyPair)?;
        Ok(RsaSigningKey {
            signer,
            public_der: cached.key_der.clone(),
            private_pem,
        })
    }

    #[test]
    fn generate_simple_self_signed_produces_valid_rsa_key_signature_and_certificate(
    ) -> Result<(), Error> {
        let cached = cached_localhost_cert()?;
        let signing_key = cached_signing_key()?;

        let key_pem = signing_key.serialize_pem();
        let trimmed_key_pem = key_pem.trim();
        assert!(trimmed_key_pem.starts_with("-----BEGIN"));
        assert!(trimmed_key_pem.contains("-----END"));
        assert!(trimmed_key_pem.ends_with("-----"));

        assert!(!signing_key.der_bytes().is_empty());
        assert_eq!(signing_key.algorithm(), &PKCS_RSA_SHA256);

        let sig = signing_key.sign(b"susi-rcgen-contract-message")?;
        assert!(!sig.is_empty());

        assert!(cached.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(!cached.cert_der.is_empty());

        let mut multi_san =
            CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;
        multi_san.serial_number = Some(SerialNumber::from_slice(&[1u8; 20]));
        let cert = multi_san.self_signed(&signing_key)?;
        assert!(cert.pem().starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(!cert.der().is_empty());
        Ok(())
    }

    #[test]
    fn consecutive_self_signed_generations_produce_distinct_keys_and_certificates(
    ) -> Result<(), Error> {
        let first = cached_localhost_cert()?;
        let second = generate_simple_self_signed(vec!["localhost".to_string()])?;
        assert!(!first.key_der.is_empty());
        assert_ne!(first.key_der.as_slice(), second.signing_key.der_bytes());
        assert_ne!(first.key_pem, second.signing_key.serialize_pem());
        assert_ne!(first.cert_der.as_slice(), second.cert.der().as_ref());
        Ok(())
    }

    #[test]
    fn rsa_signing_key_signs_empty_and_large_payloads_with_pkcs_rsa_sha256() -> Result<(), Error> {
        let signing_key = cached_signing_key()?;
        assert_eq!(signing_key.algorithm(), &PKCS_RSA_SHA256);

        let empty_sig = signing_key.sign(b"")?;
        let large_payload = vec![0xA5u8; 8192];
        let large_sig = signing_key.sign(&large_payload)?;
        assert!(!empty_sig.is_empty());
        assert!(!large_sig.is_empty());
        assert_ne!(empty_sig, large_sig);
        Ok(())
    }

    #[test]
    fn generate_simple_self_signed_rejects_non_ascii_san_and_accepts_empty_names(
    ) -> Result<(), Error> {
        assert!(generate_simple_self_signed(vec!["non-ascii-\u{00ff}.local".to_string()]).is_err());
        let empty = generate_simple_self_signed(Vec::<String>::new())?;
        assert!(!empty.cert.der().is_empty());
        Ok(())
    }

    #[test]
    fn certificate_params_new_and_date_time_ymd_construct_valid_parameters() -> Result<(), Error> {
        let mut params = CertificateParams::new(vec!["localhost".to_string()])?;
        params.not_before = date_time_ymd(2025, 1, 1);
        params.not_after = date_time_ymd(2035, 1, 1);
        assert_ne!(params.not_before, params.not_after);
        Ok(())
    }
}
