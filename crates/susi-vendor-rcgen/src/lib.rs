//! Certificate encoding by rcgen with RustCrypto RSA signing.
#![forbid(unsafe_code)]
pub use rcgen::*;
use rsa::pkcs1::EncodeRsaPublicKey;
use rsa::pkcs8::EncodePrivateKey;
use rsa::rand_core::{OsRng, RngCore};
use rsa::signature::{RandomizedSigner, SignatureEncoding};

pub struct RsaSigningKey {
    signer: rsa::pkcs1v15::SigningKey<rsa::sha2::Sha256>,
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
            .try_sign_with_rng(&mut OsRng, message)
            .map(|signature| signature.to_vec())
            .map_err(|_| Error::RemoteKeyError)
    }
}
pub fn generate_simple_self_signed(
    names: impl Into<Vec<String>>,
) -> Result<CertifiedKey<RsaSigningKey>, Error> {
    let private =
        rsa::RsaPrivateKey::new(&mut OsRng, 3072).map_err(|_| Error::KeyGenerationUnavailable)?;
    let public_der = private
        .to_public_key()
        .to_pkcs1_der()
        .map_err(|_| Error::CouldNotParseKeyPair)?
        .as_bytes()
        .to_vec();
    let private_pem = private
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .map_err(|_| Error::CouldNotParseKeyPair)?;
    let signing_key = RsaSigningKey {
        signer: rsa::pkcs1v15::SigningKey::<rsa::sha2::Sha256>::new(private),
        public_der,
        private_pem,
    };
    let mut parameters = CertificateParams::new(names)?;
    let mut serial = [0u8; 20];
    OsRng
        .try_fill_bytes(&mut serial)
        .map_err(|_| Error::KeyGenerationUnavailable)?;
    serial[0] &= 0x7f;
    serial[0] |= 1;
    parameters.serial_number = Some(SerialNumber::from_slice(&serial));
    let cert = parameters.self_signed(&signing_key)?;
    Ok(CertifiedKey { cert, signing_key })
}
