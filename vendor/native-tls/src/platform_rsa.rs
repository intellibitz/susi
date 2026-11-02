//! RSA operations provided by the installed operating-system crypto provider.
//! No RustCrypto RSA private-key arithmetic or bundled native crypto source.
#![deny(unsafe_code)]

use pkcs8::der::Decode;
use std::io;
use zeroize::Zeroizing;

#[cfg_attr(target_os = "windows", path = "platform_rsa_windows.rs")]
#[cfg_attr(target_vendor = "apple", path = "platform_rsa_macos.rs")]
#[cfg_attr(
    not(any(target_os = "windows", target_vendor = "apple")),
    path = "platform_rsa_openssl.rs"
)]
mod provider;

/// A local RSA certificate-signing key owned by the platform crypto provider.
///
/// Generation retains the existing RSA-3072/PKCS#8 identity format. Signing is
/// PKCS#1 v1.5 with SHA-256. TLS handshakes continue to use the native TLS stack.
pub struct RsaSigningKey(provider::PlatformRsaKey);

impl RsaSigningKey {
    /// Generate a fresh 3072-bit RSA key with the platform's secure RNG.
    pub fn generate() -> io::Result<Self> {
        Ok(Self(provider::PlatformRsaKey::new()?))
    }

    /// Import a PKCS#8 RSA key and verify its private/public consistency.
    ///
    /// The provider validates the imported key; a fresh signature proof also
    /// checks that its private operation agrees with the exported public key.
    pub fn from_pkcs8(pem: &[u8]) -> io::Result<Self> {
        let key = Self(provider::PlatformRsaKey::from_pkcs8(pem)?);
        key.public_key_der()?;
        let mut challenge = [0u8; 32];
        getrandom::getrandom(&mut challenge)
            .map_err(|_| io::Error::other("platform random source unavailable"))?;
        let signature = key.0.sign_sha256(&challenge)?;
        key.0.verify_sha256(&challenge, &signature)?;
        Ok(key)
    }

    /// Encode the public key as PKCS#1 DER for a certificate BIT STRING.
    pub fn public_key_der(&self) -> io::Result<Vec<u8>> {
        let der = self.0.public_key_der()?;
        let public = pkcs1::RsaPublicKey::from_der(&der).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid platform RSA public key",
            )
        })?;
        let modulus = public.modulus.as_bytes();
        let exponent = public.public_exponent.as_bytes();
        // Preserve the previous RSA public-key policy without private arithmetic.
        let value = exponent.iter().try_fold(0u64, |value, byte| {
            value.checked_mul(256)?.checked_add(u64::from(*byte))
        });
        let exponent_less_than_modulus = exponent.len() < modulus.len()
            || (exponent.len() == modulus.len() && exponent < modulus);
        let odd_modulus = matches!(modulus.last(), Some(last) if last & 1 == 1);
        let valid_exponent = matches!(value, Some(value) if (3..=(1u64 << 33) - 1).contains(&value) && value & 1 == 1);
        if modulus.len() > 512 || !odd_modulus || !exponent_less_than_modulus || !valid_exponent {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported RSA public parameters",
            ));
        }
        Ok(der)
    }

    /// Encode the private key as zeroizing PKCS#8 PEM.
    pub fn private_key_pem(&self) -> io::Result<Zeroizing<String>> {
        let bytes = Zeroizing::new(self.0.private_key_pem()?);
        let pem = std::str::from_utf8(&bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid platform key PEM"))?;
        Ok(Zeroizing::new(pem.to_owned()))
    }

    /// Sign the complete message using RSA PKCS#1 v1.5 and SHA-256.
    pub fn sign_sha256(&self, message: &[u8]) -> io::Result<Vec<u8>> {
        self.0.sign_sha256(message)
    }

    /// Verify a signature with this key's public component.
    pub fn verify_sha256(&self, message: &[u8], signature: &[u8]) -> io::Result<()> {
        self.0.verify_sha256(message, signature)
    }
}
