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
    let mut parameters = CertificateParams::new(names)?;
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

    #[test]
    fn generate_simple_self_signed_produces_valid_rsa_key_signature_and_certificate(
    ) -> Result<(), Error> {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;

        let key_pem = signing_key.serialize_pem();
        let trimmed_key_pem = key_pem.trim();
        assert!(trimmed_key_pem.starts_with("-----BEGIN"));
        assert!(trimmed_key_pem.contains("-----END"));
        assert!(trimmed_key_pem.ends_with("-----"));

        assert!(!signing_key.der_bytes().is_empty());
        assert_eq!(signing_key.algorithm(), &PKCS_RSA_SHA256);

        let sig = signing_key.sign(b"susi-rcgen-contract-message")?;
        assert!(!sig.is_empty());

        let cert_pem = cert.pem();
        assert!(cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(!cert.der().is_empty());
        Ok(())
    }
}
