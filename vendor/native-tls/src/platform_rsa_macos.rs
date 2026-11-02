//! RSA operations performed by the installed Apple Security.framework provider.
//!
//! Generated/imported keys are ephemeral: no keychain is selected and generation
//! does not set a permanent location. The existing TLS identity importer owns
//! its separate keychain lifecycle. This module performs no RSA arithmetic.
#![deny(unsafe_code)]

use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::error::CFError;
use core_foundation::string::CFString;
use pkcs8::der::Decode;
use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey};
use std::io;
use zeroize::Zeroizing;

pub(crate) struct PlatformRsaKey {
    key: SecKey,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[allow(unsafe_code)] // Narrow Security.framework import and owned CF object conversion boundary.
fn import_private_key(pkcs1_der: &[u8]) -> io::Result<SecKey> {
    use security_framework_sys::item::{
        kSecAttrKeyClass, kSecAttrKeyClassPrivate, kSecAttrKeyType, kSecAttrKeyTypeRSA,
    };
    use security_framework_sys::key::SecKeyCreateWithData;

    let data = CFData::from_buffer(pkcs1_der);
    // SAFETY: Apple exports these immutable, non-null CFString constants for
    // the process lifetime. Get-rule wrappers retain them. The dictionary
    // owns retained copies and no permanent/keychain attribute is requested.
    let attributes = unsafe {
        CFDictionary::from_CFType_pairs(&[
            (
                CFString::wrap_under_get_rule(kSecAttrKeyType),
                CFString::wrap_under_get_rule(kSecAttrKeyTypeRSA),
            ),
            (
                CFString::wrap_under_get_rule(kSecAttrKeyClass),
                CFString::wrap_under_get_rule(kSecAttrKeyClassPrivate),
            ),
        ])
    };
    let mut error = std::ptr::null_mut();
    // SAFETY: data and attributes remain live for the complete synchronous
    // call; error is a valid out-pointer. Non-null result/error values follow
    // Core Foundation's Create Rule and are wrapped exactly once below.
    let raw = unsafe {
        SecKeyCreateWithData(
            data.as_concrete_TypeRef(),
            attributes.as_concrete_TypeRef(),
            &mut error,
        )
    };
    let failed = !error.is_null();
    if failed {
        // SAFETY: a non-null error was created for this call; the wrapper
        // takes its sole owned reference and releases it at scope exit.
        drop(unsafe { CFError::wrap_under_create_rule(error) });
    }
    if raw.is_null() {
        return Err(invalid("Security.framework rejected the RSA private key"));
    }
    // SAFETY: raw was checked non-null and is the owned SecKeyRef returned by
    // this Create function. SecKey owns/releases that reference from here.
    let key = unsafe { SecKey::wrap_under_create_rule(raw) };
    if failed {
        return Err(invalid("Security.framework rejected the RSA private key"));
    }
    Ok(key)
}

impl PlatformRsaKey {
    pub(crate) fn new() -> io::Result<Self> {
        let mut options = GenerateKeyOptions::default();
        options.set_key_type(KeyType::rsa()).set_size_in_bits(3072);
        let key = SecKey::new(&options)
            .map_err(|_| invalid("Security.framework RSA key generation failed"))?;
        Ok(Self { key })
    }

    pub(crate) fn from_pkcs8(pem: &[u8]) -> io::Result<Self> {
        let text = std::str::from_utf8(pem).map_err(|_| invalid("invalid private key PEM"))?;
        let (label, document) = pkcs8::SecretDocument::from_pem(text)
            .map_err(|_| invalid("invalid PKCS#8 private key PEM"))?;
        if label != "PRIVATE KEY" {
            return Err(invalid("expected PKCS#8 PRIVATE KEY PEM"));
        }
        let info = pkcs8::PrivateKeyInfo::try_from(document.as_bytes())
            .map_err(|_| invalid("invalid PKCS#8 private key"))?;
        if info.algorithm.oid != pkcs1::ALGORITHM_OID
            || info.algorithm.parameters_any().ok() != Some(pkcs8::der::asn1::Null.into())
        {
            return Err(invalid("expected RSA PKCS#8 algorithm and NULL parameters"));
        }
        let private = pkcs1::RsaPrivateKey::from_der(info.private_key)
            .map_err(|_| invalid("invalid RSA private key encoding"))?;
        if private.version() != pkcs1::Version::TwoPrime {
            return Err(invalid("multi-prime RSA identities are unsupported"));
        }

        // Explicit RSA/private import supports Apple targets without the
        // macOS-only SecItemImport API or selecting a persistent keychain.
        // Structural checks do not replace the shared signature proof.
        let key = import_private_key(info.private_key)?;
        Ok(Self { key })
    }

    pub(crate) fn public_key_der(&self) -> io::Result<Vec<u8>> {
        let public = self
            .key
            .public_key()
            .ok_or_else(|| invalid("Security.framework returned no RSA public key"))?;
        let der = public
            .external_representation()
            .ok_or_else(|| invalid("Security.framework RSA public key export failed"))?
            .to_vec();
        pkcs1::RsaPublicKey::from_der(&der)
            .map_err(|_| invalid("Security.framework returned invalid RSA public key DER"))?;
        Ok(der)
    }

    pub(crate) fn private_key_pem(&self) -> io::Result<Vec<u8>> {
        // The framework owns the temporary CFData allocation and its release.
        // The Rust-owned DER/PEM copies are zeroized on drop. The caller owns
        // and must zeroize the returned PEM buffer after persisting/using it.
        let private_der = Zeroizing::new(
            self.key
                .external_representation()
                .ok_or_else(|| invalid("Security.framework RSA private key export failed"))?
                .to_vec(),
        );
        pkcs1::RsaPrivateKey::from_der(&private_der)
            .map_err(|_| invalid("Security.framework returned invalid RSA private key DER"))?;
        let info = pkcs8::PrivateKeyInfo::new(pkcs1::ALGORITHM_ID, &private_der);
        let document = pkcs8::SecretDocument::try_from(info)
            .map_err(|_| invalid("RSA PKCS#8 encoding failed"))?;
        let pem = document
            .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
            .map_err(|_| invalid("RSA PKCS#8 PEM encoding failed"))?;
        Ok(pem.as_bytes().to_vec())
    }

    pub(crate) fn sign_sha256(&self, message: &[u8]) -> io::Result<Vec<u8>> {
        self.key
            .create_signature(Algorithm::RSASignatureMessagePKCS1v15SHA256, message)
            .map_err(|_| invalid("Security.framework RSA SHA-256 signing failed"))
    }

    pub(crate) fn verify_sha256(&self, message: &[u8], signature: &[u8]) -> io::Result<()> {
        let public = self
            .key
            .public_key()
            .ok_or_else(|| invalid("Security.framework returned no RSA public key"))?;
        match public.verify_signature(
            Algorithm::RSASignatureMessagePKCS1v15SHA256,
            message,
            signature,
        ) {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(invalid(
                "Security.framework RSA signature verification failed",
            )),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Native-provider regression tests report the failing operation.
mod tests {
    use super::*;
    use pkcs8::der::Encode;

    #[test]
    fn platform_rsa_pkcs8_round_trip_and_signature_integrity() {
        let key = PlatformRsaKey::new().unwrap();
        let public_der = key.public_key_der().unwrap();
        let public = pkcs1::RsaPublicKey::from_der(&public_der).unwrap();
        assert_eq!(public.modulus.as_bytes().len(), 384);
        let pem = Zeroizing::new(key.private_key_pem().unwrap());
        let loaded = PlatformRsaKey::from_pkcs8(&pem).unwrap();
        assert_eq!(loaded.public_key_der().unwrap(), public_der);
        let signature = loaded.sign_sha256(b"Susi macOS identity proof").unwrap();
        key.verify_sha256(b"Susi macOS identity proof", &signature)
            .unwrap();
        assert!(key.verify_sha256(b"changed message", &signature).is_err());
    }

    #[test]
    fn platform_rsa_rejects_corrupted_private_component_with_same_public_key() {
        let key = PlatformRsaKey::new().unwrap();
        let pem = Zeroizing::new(key.private_key_pem().unwrap());
        let (_, document) =
            pkcs8::SecretDocument::from_pem(std::str::from_utf8(&pem).unwrap()).unwrap();
        let info = pkcs8::PrivateKeyInfo::try_from(document.as_bytes()).unwrap();
        let private = pkcs1::RsaPrivateKey::from_der(info.private_key).unwrap();

        for corrupt_exponent in [true, false] {
            let mut corrupted = private.clone();
            let invalid_component = pkcs1::UintRef::new(&[1]).unwrap();
            if corrupt_exponent {
                corrupted.private_exponent = invalid_component;
            } else {
                corrupted.prime1 = invalid_component;
            }
            let encoded = Zeroizing::new(corrupted.to_der().unwrap());
            let damaged_info = pkcs8::PrivateKeyInfo::new(pkcs1::ALGORITHM_ID, &encoded);
            let damaged_document = pkcs8::SecretDocument::try_from(damaged_info).unwrap();
            let damaged_pem = damaged_document
                .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
                .unwrap();
            // A provider may reject at import or at proof-of-possession. Both
            // paths must fail closed before the identity can be accepted.
            let invalid = PlatformRsaKey::from_pkcs8(damaged_pem.as_bytes()).and_then(|damaged| {
                let signature = damaged.sign_sha256(b"Susi invalid identity proof")?;
                damaged.verify_sha256(b"Susi invalid identity proof", &signature)
            });
            assert!(
                invalid.is_err(),
                "provider accepted corrupted RSA key components"
            );
        }
    }
}
