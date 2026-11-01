//! OS TLS behind the existing workspace transport facade.
//! Windows uses Schannel. Unix uses platform OpenSSL, not a Rust TLS engine.
#![forbid(unsafe_code)]
pub use native_tls;
pub use rustls_pki_types as pki_types;
pub use tokio_native_tls::{TlsAcceptor, TlsConnector};
pub mod server {
    pub use tokio_native_tls::TlsStream;
}

pub fn acceptor_from_pem(cert: &[u8], key: &[u8]) -> std::io::Result<TlsAcceptor> {
    validate_identity(cert, key)?;
    let invalid = |error| std::io::Error::new(std::io::ErrorKind::InvalidData, error);
    let identity = native_tls::Identity::from_pkcs8(cert, key).map_err(invalid)?;
    let mut builder = native_tls::TlsAcceptor::builder(identity);
    builder.min_protocol_version(Some(native_tls::Protocol::Tlsv12));
    builder.accept_alpn(&["h2", "http/1.1"]);
    Ok(builder.build().map_err(invalid)?.into())
}

fn validate_identity(cert: &[u8], key: &[u8]) -> std::io::Result<()> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::pkcs8::DecodePrivateKey;
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let leaf = x509_cert::Certificate::load_pem_chain(cert)
        .map_err(|_| invalid("invalid certificate PEM chain"))?
        .into_iter()
        .next()
        .ok_or_else(|| invalid("empty certificate chain"))?;
    let public = rsa::RsaPublicKey::from_pkcs1_der(
        leaf.tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or_else(|| invalid("invalid certificate public key"))?,
    )
    .map_err(|_| invalid("native PKCS#8 identity requires an RSA certificate"))?;
    let private = rsa::RsaPrivateKey::from_pkcs8_pem(
        std::str::from_utf8(key).map_err(|_| invalid("invalid private key PEM"))?,
    )
    .map_err(|_| invalid("invalid RSA PKCS#8 private key"))?;
    if private.to_public_key() != public {
        return Err(invalid("certificate and private key do not match"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test failures must identify the failing operation.
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

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
}
