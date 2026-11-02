//! Platform OpenSSL provider. OpenSSL source is never bundled by this boundary.
#![forbid(unsafe_code)]

use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Private};
use openssl::rsa::{Padding, Rsa};
use openssl::sign::{Signer, Verifier};
use pkcs8::der::Decode;
use std::io;
use zeroize::Zeroizing;

pub(crate) struct PlatformRsaKey(PKey<Private>);

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

impl PlatformRsaKey {
    pub(crate) fn new() -> io::Result<Self> {
        let rsa = Rsa::generate(3072).map_err(invalid)?;
        Ok(Self(PKey::from_rsa(rsa).map_err(invalid)?))
    }

    pub(crate) fn from_pkcs8(pem: &[u8]) -> io::Result<Self> {
        // Strictly decode PKCS#8 before calling the more permissive PEM importer.
        // SecretDocument zeroizes its decoded private bytes when dropped.
        let (label, document) = pkcs8::SecretDocument::from_pem(
            std::str::from_utf8(pem).map_err(|_| invalid("invalid private key PEM"))?,
        )
        .map_err(invalid)?;
        if label != "PRIVATE KEY" {
            return Err(invalid("identity requires a PKCS#8 PRIVATE KEY"));
        }
        let info = pkcs8::PrivateKeyInfo::from_der(document.as_bytes()).map_err(invalid)?;
        if info.algorithm != pkcs1::ALGORITHM_ID {
            return Err(invalid("identity requires an RSA PKCS#8 private key"));
        }
        let encoded = pkcs1::RsaPrivateKey::from_der(info.private_key).map_err(invalid)?;
        if encoded.other_prime_infos.is_some() {
            return Err(invalid("multi-prime RSA identities are unsupported"));
        }
        let key = PKey::private_key_from_pkcs8(document.as_bytes()).map_err(invalid)?;
        let rsa = key.rsa().map_err(invalid)?;
        if !rsa.check_key().map_err(invalid)? {
            return Err(invalid("RSA private key consistency check failed"));
        }
        Ok(Self(key))
    }

    pub(crate) fn public_key_der(&self) -> io::Result<Vec<u8>> {
        self.0
            .rsa()
            .map_err(invalid)?
            .public_key_to_der_pkcs1()
            .map_err(invalid)
    }

    pub(crate) fn private_key_pem(&self) -> io::Result<Vec<u8>> {
        // i2d writes directly into an owned Rust buffer. Avoid a temporary
        // OpenSSL memory BIO containing unprotected PEM text.
        let private = Zeroizing::new(
            self.0
                .rsa()
                .map_err(invalid)?
                .private_key_to_der()
                .map_err(invalid)?,
        );
        let info = pkcs8::PrivateKeyInfo::new(pkcs1::ALGORITHM_ID, &private);
        let document = pkcs8::SecretDocument::try_from(info).map_err(invalid)?;
        let pem = document
            .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
            .map_err(invalid)?;
        Ok(pem.as_bytes().to_vec())
    }

    pub(crate) fn sign_sha256(&self, message: &[u8]) -> io::Result<Vec<u8>> {
        let mut signer = Signer::new(MessageDigest::sha256(), &self.0).map_err(invalid)?;
        signer.set_rsa_padding(Padding::PKCS1).map_err(invalid)?;
        signer.update(message).map_err(invalid)?;
        signer.sign_to_vec().map_err(invalid)
    }

    pub(crate) fn verify_sha256(&self, message: &[u8], signature: &[u8]) -> io::Result<()> {
        // Export/reimport the public component: verification must not rely on
        // a provider accidentally consulting the private operation being tested.
        let rsa = Rsa::public_key_from_der_pkcs1(&self.public_key_der()?).map_err(invalid)?;
        let public = PKey::from_rsa(rsa).map_err(invalid)?;
        let mut verifier = Verifier::new(MessageDigest::sha256(), &public).map_err(invalid)?;
        verifier.set_rsa_padding(Padding::PKCS1).map_err(invalid)?;
        verifier.update(message).map_err(invalid)?;
        if verifier.verify(signature).map_err(invalid)? {
            Ok(())
        } else {
            Err(invalid("RSA signature consistency check failed"))
        }
    }
}
