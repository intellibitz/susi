//! OS TLS behind the existing workspace transport facade.
//! Windows uses Schannel. Unix uses platform OpenSSL, not a Rust TLS engine.
#![forbid(unsafe_code)]
pub use native_tls;
pub use rustls_pki_types as pki_types;
pub use tokio_native_tls::TlsConnector;
pub mod server {
    pub use tokio_native_tls::TlsStream;
}

/// A platform TLS acceptor usable from both async and blocking HTTP hosts.
///
/// The async half is the `tokio-native-tls` adapter used by the runtime
/// servers.  The blocking half keeps the same platform identity available to
/// the std-thread HTTP server; using the adapter's inner stream directly from
/// a blocking thread would violate its task-local waker contract.
#[derive(Clone)]
pub struct TlsAcceptor {
    asynchronous: tokio_native_tls::TlsAcceptor,
    blocking: native_tls::TlsAcceptor,
}

impl TlsAcceptor {
    /// Accept a client stream on a Tokio runtime.
    pub async fn accept<S>(&self, stream: S) -> Result<server::TlsStream<S>, native_tls::Error>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        self.asynchronous.accept(stream).await
    }

    /// Accept a client stream on a blocking connection thread.
    pub fn accept_blocking<S>(
        &self,
        stream: S,
    ) -> Result<native_tls::TlsStream<S>, native_tls::HandshakeError<S>>
    where
        S: std::io::Read + std::io::Write,
    {
        self.blocking.accept(stream)
    }
}

pub fn acceptor_from_pem(cert: &[u8], key: &[u8]) -> std::io::Result<TlsAcceptor> {
    validate_identity(cert, key)?;
    let invalid = |error| std::io::Error::new(std::io::ErrorKind::InvalidData, error);
    let identity = native_tls::Identity::from_pkcs8(cert, key).map_err(invalid)?;
    let mut builder = native_tls::TlsAcceptor::builder(identity);
    builder.min_protocol_version(Some(native_tls::Protocol::Tlsv12));
    builder.accept_alpn(&["h2", "http/1.1"]);
    let blocking = builder.build().map_err(invalid)?;
    Ok(TlsAcceptor {
        asynchronous: blocking.clone().into(),
        blocking,
    })
}

fn validate_identity(cert: &[u8], key: &[u8]) -> std::io::Result<()> {
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let leaf = x509_cert::Certificate::load_pem_chain(cert)
        .map_err(|_| invalid("invalid certificate PEM chain"))?
        .into_iter()
        .next()
        .ok_or_else(|| invalid("empty certificate chain"))?;
    let public_info = leaf.tbs_certificate.subject_public_key_info;
    // DER and the explicit RSA OID identify the certificate's public key. The
    // platform import validates private components and proves signing ability;
    // comparing serialized n/e alone would not validate a corrupted private key.
    if public_info.algorithm.oid.to_string() != "1.2.840.113549.1.1.1" {
        return Err(invalid(
            "native PKCS#8 identity requires an RSA certificate",
        ));
    }
    let public = public_info
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| invalid("invalid certificate public key"))?;
    let private = native_tls::RsaSigningKey::from_pkcs8(key)?;
    if private.public_key_der()?.as_slice() != public {
        return Err(invalid("certificate and private key do not match"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test failures must identify the failing operation.
mod tests {
    use super::*;
    use pkcs8::der::{Decode, Encode};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;
    use zeroize::Zeroizing;

    #[test]
    fn accepts_unchanged_identity_created_by_the_accepted_rsa_implementation() {
        let cert = include_bytes!("../tests/fixtures/accepted-rsa-cert.pem");
        let key = include_bytes!("../tests/fixtures/accepted-rsa-key.pem");
        assert!(acceptor_from_pem(cert, key).is_ok());
        assert!(roundtrip(cert, key, true, "localhost"));
    }

    #[test]
    fn platform_rsa_roundtrip_preserves_3072_bits_and_signature_integrity() {
        let original = native_tls::RsaSigningKey::generate().unwrap();
        let public_der = original.public_key_der().unwrap();
        let public = pkcs1::RsaPublicKey::from_der(&public_der).unwrap();
        assert_eq!(public.modulus.as_bytes().len(), 384);
        assert_eq!(public.public_exponent.as_bytes(), &[1, 0, 1]);
        let pem = original.private_key_pem().unwrap();
        let loaded = native_tls::RsaSigningKey::from_pkcs8(pem.as_bytes()).unwrap();
        assert_eq!(loaded.public_key_der().unwrap(), public_der);
        let mut signature = loaded.sign_sha256(b"Susi platform key proof").unwrap();
        original
            .verify_sha256(b"Susi platform key proof", &signature)
            .unwrap();
        assert!(original
            .verify_sha256(b"modified message", &signature)
            .is_err());
        signature[0] ^= 1;
        assert!(original
            .verify_sha256(b"Susi platform key proof", &signature)
            .is_err());
    }

    #[test]
    fn platform_rsa_rejects_malformed_non_rsa_and_corrupt_private_keys() {
        assert!(native_tls::RsaSigningKey::from_pkcs8(b"malformed").is_err());
        let original = native_tls::RsaSigningKey::generate().unwrap();
        let pem = original.private_key_pem().unwrap();
        let (_, document) = pkcs8::SecretDocument::from_pem(&pem).unwrap();
        let info = pkcs8::PrivateKeyInfo::from_der(document.as_bytes()).unwrap();
        let private = pkcs1::RsaPrivateKey::from_der(info.private_key).unwrap();
        for field in 0..6 {
            let mut damaged = private.clone();
            let one = pkcs1::UintRef::new(&[1]).unwrap();
            match field {
                0 => damaged.private_exponent = one,
                1 => damaged.prime1 = one,
                2 => damaged.prime2 = one,
                3 => damaged.exponent1 = one,
                4 => damaged.exponent2 = one,
                _ => damaged.coefficient = one,
            }
            let damaged_der = Zeroizing::new(damaged.to_der().unwrap());
            let damaged_info = pkcs8::PrivateKeyInfo::new(pkcs1::ALGORITHM_ID, &damaged_der);
            let damaged_document = pkcs8::SecretDocument::try_from(damaged_info).unwrap();
            let damaged_pem = damaged_document
                .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
                .unwrap();
            assert!(
                native_tls::RsaSigningKey::from_pkcs8(damaged_pem.as_bytes()).is_err(),
                "corrupt private field {field} accepted"
            );
        }
        let mut wrong_algorithm = info.clone();
        wrong_algorithm.algorithm.oid = pkcs8::ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
        let wrong_document = pkcs8::SecretDocument::try_from(wrong_algorithm).unwrap();
        let wrong_pem = wrong_document
            .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
            .unwrap();
        assert!(native_tls::RsaSigningKey::from_pkcs8(wrong_pem.as_bytes()).is_err());
        let pkcs1_pem = document
            .to_pem("RSA PRIVATE KEY", pkcs8::LineEnding::LF)
            .unwrap();
        assert!(native_tls::RsaSigningKey::from_pkcs8(pkcs1_pem.as_bytes()).is_err());
    }

    fn roundtrip(cert: &[u8], key: &[u8], trust: bool, host: &str) -> bool {
        let identity = native_tls::Identity::from_pkcs8(cert, key).unwrap();
        let mut builder = native_tls::TlsAcceptor::builder(identity);
        builder.accept_alpn(&["http/1.1"]);
        let acceptor = builder.build().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            if let Ok(mut tls) = acceptor.accept(stream).map_err(|error| {
                eprintln!("server handshake: {error}");
                error
            }) {
                let mut request = [0u8; 4];
                if tls.read_exact(&mut request).is_ok() {
                    tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                        .unwrap();
                }
            }
        });
        let mut connector = native_tls::TlsConnector::builder();
        connector.disable_built_in_roots(true);
        connector.request_alpns(&["http/1.1"]);
        if trust {
            connector.add_root_certificate(native_tls::Certificate::from_pem(cert).unwrap());
        }
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let result = match connector.build().unwrap().connect(host, stream) {
            Ok(mut tls) => {
                assert_eq!(tls.negotiated_alpn().unwrap(), Some(b"http/1.1".to_vec()));
                tls.write_all(b"GET ").unwrap();
                let mut response = [0u8; 12];
                tls.read_exact(&mut response).unwrap();
                &response == b"HTTP/1.1 200"
            }
            Err(error) => {
                eprintln!("client handshake: {error}");
                false
            }
        };
        server.join().unwrap();
        result
    }

    #[test]
    fn platform_tls_validates_trust_hostname_and_key_and_serves_https() {
        let generated =
            susi_vendor_rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = generated.cert.pem();
        let key = generated.signing_key.serialize_pem();
        assert!(acceptor_from_pem(cert.as_bytes(), key.as_bytes()).is_ok());
        assert!(roundtrip(
            cert.as_bytes(),
            key.as_bytes(),
            true,
            "localhost"
        ));
        assert!(!roundtrip(
            cert.as_bytes(),
            key.as_bytes(),
            false,
            "localhost"
        ));
        assert!(!roundtrip(
            cert.as_bytes(),
            key.as_bytes(),
            true,
            "wrong.invalid"
        ));
        assert!(acceptor_from_pem(b"malformed", key.as_bytes()).is_err());
        let mut expired =
            susi_vendor_rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        expired.serial_number = Some(42u64.into());
        expired.not_before = susi_vendor_rcgen::date_time_ymd(1999, 1, 1);
        expired.not_after = susi_vendor_rcgen::date_time_ymd(2000, 1, 1);
        let expired = expired.self_signed(&generated.signing_key).unwrap().pem();
        assert!(!roundtrip(
            expired.as_bytes(),
            key.as_bytes(),
            true,
            "localhost"
        ));

        let other =
            susi_vendor_rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        assert!(acceptor_from_pem(
            cert.as_bytes(),
            other.signing_key.serialize_pem().as_bytes()
        )
        .is_err());
    }

    #[test]
    fn acceptor_from_pem_rejects_malformed_private_key_with_valid_certificate() {
        let generated =
            susi_vendor_rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = generated.cert.pem();
        assert!(acceptor_from_pem(cert.as_bytes(), b"not-a-valid-private-key-pem").is_err());
    }
}
