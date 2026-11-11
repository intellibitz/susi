//! OS TLS behind the existing workspace transport facade.
//! Windows uses Schannel. Unix uses platform OpenSSL, not a Rust TLS engine.
#![forbid(unsafe_code)]
pub use native_tls;
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

/// ASN.1 DER value bytes for OID `1.2.840.113549.1.1.1` (`rsaEncryption`, RFC 3279 §2.3.1).
const OID_RSA_ENCRYPTION_DER: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

/// Decode all `CERTIFICATE` blocks from an RFC 7468 PEM byte slice into raw X.509 DER
/// certificates, rejecting malformed base64, unclosed/mismatched PEM framing, or empty chains.
pub fn parse_certificate_pem_chain(pem: &[u8]) -> std::io::Result<Vec<Vec<u8>>> {
    let invalid =
        |message: &'static str| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let text = std::str::from_utf8(pem).map_err(|_| invalid("invalid certificate PEM chain"))?;
    let mut certs = Vec::new();
    let mut active_label: Option<&str> = None;
    let mut b64_buf = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r').trim();
        if let Some(label) = active_label {
            if let Some(rest) = line.strip_prefix("-----END ") {
                let end_label = rest
                    .strip_suffix("-----")
                    .ok_or_else(|| invalid("invalid certificate PEM chain"))?;
                if end_label != label || end_label.is_empty() {
                    return Err(invalid("invalid certificate PEM chain"));
                }
                if label == "CERTIFICATE" {
                    let der = decode_base64_strict(&b64_buf)
                        .ok_or_else(|| invalid("invalid certificate PEM chain"))?;
                    if der.is_empty() {
                        return Err(invalid("invalid certificate PEM chain"));
                    }
                    certs.push(der);
                }
                b64_buf.clear();
                active_label = None;
            } else if line.starts_with("-----") {
                return Err(invalid("invalid certificate PEM chain"));
            } else if label == "CERTIFICATE" {
                for &byte in line.as_bytes() {
                    if !byte.is_ascii_whitespace() {
                        b64_buf.push(byte);
                    }
                }
            }
        } else if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            let label = rest
                .strip_suffix("-----")
                .ok_or_else(|| invalid("invalid certificate PEM chain"))?;
            if label.is_empty() {
                return Err(invalid("invalid certificate PEM chain"));
            }
            b64_buf.clear();
            active_label = Some(label);
        } else if line.starts_with("-----END ") {
            return Err(invalid("invalid certificate PEM chain"));
        }
    }

    if active_label.is_some() {
        return Err(invalid("invalid certificate PEM chain"));
    }
    if certs.is_empty() {
        return Err(invalid("empty certificate chain"));
    }
    Ok(certs)
}

fn decode_base64_val(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn decode_base64_strict(input: &[u8]) -> Option<Vec<u8>> {
    let (chunks, rem) = input.as_chunks::<4>();
    if chunks.is_empty() || !rem.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(chunks.len() * 3);
    let total_chunks = chunks.len();
    for (idx, chunk) in chunks.iter().enumerate() {
        let is_last = idx + 1 == total_chunks;
        let v0 = decode_base64_val(chunk[0])?;
        let v1 = decode_base64_val(chunk[1])?;
        match (chunk[2], chunk[3]) {
            (b'=', b'=') if is_last => {
                if (v1 & 0x0f) != 0 {
                    return None;
                }
                out.push((v0 << 2) | (v1 >> 4));
            }
            (c2, b'=') if is_last && c2 != b'=' => {
                let v2 = decode_base64_val(c2)?;
                if (v2 & 0x03) != 0 {
                    return None;
                }
                out.push((v0 << 2) | (v1 >> 4));
                out.push((v1 << 4) | (v2 >> 2));
            }
            (c2, c3) if c2 != b'=' && c3 != b'=' => {
                let v2 = decode_base64_val(c2)?;
                let v3 = decode_base64_val(c3)?;
                out.push((v0 << 2) | (v1 >> 4));
                out.push((v1 << 4) | (v2 >> 2));
                out.push((v2 << 6) | v3);
            }
            _ => return None,
        }
    }
    Some(out)
}

struct DerTlv<'a> {
    tag: u8,
    value: &'a [u8],
    rem: &'a [u8],
}

fn read_der_tlv(input: &[u8]) -> std::io::Result<DerTlv<'_>> {
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid X.509 DER");
    if input.len() < 2 {
        return Err(invalid());
    }
    let tag = input[0];
    if (tag & 0x1f) == 0x1f {
        return Err(invalid());
    }
    let first_len = input[1];
    let (header_len, content_len) = if (first_len & 0x80) == 0 {
        (2usize, usize::from(first_len))
    } else {
        let num_octets = usize::from(first_len & 0x7f);
        if num_octets == 0 || num_octets > 4 || input.len() < 2 + num_octets {
            return Err(invalid());
        }
        if input[2] == 0 {
            return Err(invalid());
        }
        let mut len = 0usize;
        for &b in &input[2..2 + num_octets] {
            len = (len << 8) | usize::from(b);
        }
        if len < 128 {
            return Err(invalid());
        }
        (2 + num_octets, len)
    };
    let end = header_len.checked_add(content_len).ok_or_else(invalid)?;
    if input.len() < end {
        return Err(invalid());
    }
    Ok(DerTlv {
        tag,
        value: &input[header_len..end],
        rem: &input[end..],
    })
}

/// Parse an RFC 5280 DER `Certificate` and extract `(algorithm_oid_der, subject_public_key_bit_string_value)`
/// from `tbsCertificate.subjectPublicKeyInfo`.
fn parse_certificate_spki(cert_der: &[u8]) -> std::io::Result<(&[u8], &[u8])> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid certificate PEM chain",
        )
    };
    let cert_seq = read_der_tlv(cert_der).map_err(|_| invalid())?;
    if cert_seq.tag != 0x30 || !cert_seq.rem.is_empty() {
        return Err(invalid());
    }
    let tbs_seq = read_der_tlv(cert_seq.value).map_err(|_| invalid())?;
    if tbs_seq.tag != 0x30 {
        return Err(invalid());
    }
    let sig_alg = read_der_tlv(tbs_seq.rem).map_err(|_| invalid())?;
    if sig_alg.tag != 0x30 {
        return Err(invalid());
    }
    let sig_val = read_der_tlv(sig_alg.rem).map_err(|_| invalid())?;
    if sig_val.tag != 0x03
        || sig_val.value.is_empty()
        || sig_val.value[0] > 7
        || !sig_val.rem.is_empty()
    {
        return Err(invalid());
    }

    // TBSCertificate: optional `[0] EXPLICIT Version`, then serialNumber, signature,
    // issuer, validity, subject, subjectPublicKeyInfo.
    let mut tbs = tbs_seq.value;
    let first = read_der_tlv(tbs).map_err(|_| invalid())?;
    if first.tag == 0xa0 {
        let ver = read_der_tlv(first.value).map_err(|_| invalid())?;
        if ver.tag != 0x02 || ver.value.is_empty() || !ver.rem.is_empty() {
            return Err(invalid());
        }
        tbs = first.rem;
    }
    let serial = read_der_tlv(tbs).map_err(|_| invalid())?;
    if serial.tag != 0x02 || serial.value.is_empty() {
        return Err(invalid());
    }
    tbs = serial.rem;
    for expected_tag in [0x30u8, 0x30, 0x30, 0x30] {
        let field = read_der_tlv(tbs).map_err(|_| invalid())?;
        if field.tag != expected_tag {
            return Err(invalid());
        }
        tbs = field.rem;
    }
    let spki = read_der_tlv(tbs).map_err(|_| invalid())?;
    if spki.tag != 0x30 {
        return Err(invalid());
    }
    let alg = read_der_tlv(spki.value).map_err(|_| invalid())?;
    if alg.tag != 0x30 {
        return Err(invalid());
    }
    let oid = read_der_tlv(alg.value).map_err(|_| invalid())?;
    if oid.tag != 0x06 || oid.value.is_empty() {
        return Err(invalid());
    }
    if !oid.rem.is_empty() {
        let params = read_der_tlv(oid.rem).map_err(|_| invalid())?;
        if !params.rem.is_empty() {
            return Err(invalid());
        }
    }
    let spk = read_der_tlv(alg.rem).map_err(|_| invalid())?;
    if spk.tag != 0x03 || spk.value.is_empty() || spk.value[0] > 7 || !spk.rem.is_empty() {
        return Err(invalid());
    }
    Ok((oid.value, spk.value))
}

fn validate_identity(cert: &[u8], key: &[u8]) -> std::io::Result<()> {
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let certs = parse_certificate_pem_chain(cert)?;
    let leaf_der = certs
        .first()
        .ok_or_else(|| invalid("empty certificate chain"))?;
    let (oid_bytes, spk_bits) = parse_certificate_spki(leaf_der)?;
    // DER and the explicit RSA OID identify the certificate's public key. The
    // platform import validates private components and proves signing ability;
    // comparing serialized n/e alone would not validate a corrupted private key.
    if oid_bytes != OID_RSA_ENCRYPTION_DER {
        return Err(invalid(
            "native PKCS#8 identity requires an RSA certificate",
        ));
    }
    if spk_bits[0] != 0 || spk_bits.len() < 2 {
        return Err(invalid("invalid certificate public key"));
    }
    let public = &spk_bits[1..];
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

    fn der_tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let len = body.len();
        if len < 128 {
            out.push(len as u8);
        } else if len <= 0xff {
            out.push(0x81);
            out.push(len as u8);
        } else {
            out.push(0x82);
            out.push((len >> 8) as u8);
            out.push((len & 0xff) as u8);
        }
        out.extend_from_slice(body);
        out
    }

    fn encode_pem_cert(der: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for chunk in der.chunks(3) {
            let b0 = chunk[0];
            let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
            b64.push(ALPHABET[usize::from(b0 >> 2)] as char);
            b64.push(ALPHABET[usize::from(((b0 & 0x03) << 4) | (b1 >> 4))] as char);
            b64.push(if chunk.len() > 1 {
                ALPHABET[usize::from(((b1 & 0x0f) << 2) | (b2 >> 6))] as char
            } else {
                '='
            });
            b64.push(if chunk.len() > 2 {
                ALPHABET[usize::from(b2 & 0x3f)] as char
            } else {
                '='
            });
        }
        format!("-----BEGIN CERTIFICATE-----\n{b64}\n-----END CERTIFICATE-----\n")
    }

    fn build_synthetic_x509_der(
        include_v3_version: bool,
        spki_oid_der: &[u8],
        unused_spk_bits: u8,
        spk_payload: &[u8],
    ) -> Vec<u8> {
        let mut tbs_items = Vec::new();
        if include_v3_version {
            let v3 = der_tlv(0x02, &[0x02]);
            tbs_items.extend_from_slice(&der_tlv(0xa0, &v3));
        }
        tbs_items.extend_from_slice(&der_tlv(0x02, &[0x01])); // serialNumber
        let sig_alg = {
            let mut body = der_tlv(0x06, &OID_RSA_ENCRYPTION_DER);
            body.extend_from_slice(&[0x05, 0x00]);
            der_tlv(0x30, &body)
        };
        tbs_items.extend_from_slice(&sig_alg); // signature
        tbs_items.extend_from_slice(&der_tlv(0x30, &[])); // issuer
        tbs_items.extend_from_slice(&der_tlv(0x30, &[])); // validity
        tbs_items.extend_from_slice(&der_tlv(0x30, &[])); // subject
        let spki_alg = {
            let mut body = der_tlv(0x06, spki_oid_der);
            body.extend_from_slice(&[0x05, 0x00]);
            der_tlv(0x30, &body)
        };
        let mut bit_string = vec![unused_spk_bits];
        bit_string.extend_from_slice(spk_payload);
        let mut spki_body = spki_alg;
        spki_body.extend_from_slice(&der_tlv(0x03, &bit_string));
        tbs_items.extend_from_slice(&der_tlv(0x30, &spki_body));

        let mut cert_body = der_tlv(0x30, &tbs_items);
        cert_body.extend_from_slice(&sig_alg);
        cert_body.extend_from_slice(&der_tlv(0x03, &[0x00, 0xaa]));
        der_tlv(0x30, &cert_body)
    }

    #[test]
    fn pem_chain_and_x509_spki_parser_enforces_strict_rfc7468_and_rfc5280_invariants() {
        let cert1 = include_bytes!("../tests/fixtures/accepted-rsa-cert.pem");
        let key1 = include_bytes!("../tests/fixtures/accepted-rsa-key.pem");
        let generated =
            susi_vendor_rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let mut chained = Vec::from(cert1.as_slice());
        chained.extend_from_slice(
            b"\n-----BEGIN TRUST COMMENT-----\nAQID\n-----END TRUST COMMENT-----\n",
        );
        chained.extend_from_slice(generated.cert.pem().as_bytes());
        let decoded = parse_certificate_pem_chain(&chained).unwrap();
        assert_eq!(decoded.len(), 2);

        // Unclosed PEM framing, stray END, empty chain, and non-canonical base64 pad bits fail closed.
        assert!(parse_certificate_pem_chain(b"").is_err());
        assert!(parse_certificate_pem_chain(b"-----BEGIN CERTIFICATE-----\nAQID\n").is_err());
        assert!(parse_certificate_pem_chain(b"-----END CERTIFICATE-----\n").is_err());
        assert!(parse_certificate_pem_chain(
            b"-----BEGIN CERTIFICATE-----\nAB==\n-----END CERTIFICATE-----\n"
        )
        .is_err());

        // Default X.509 v1 (omitted `[0] EXPLICIT Version`) with matching RSA public key succeeds.
        let signer = native_tls::RsaSigningKey::from_pkcs8(key1).unwrap();
        let pub_der = signer.public_key_der().unwrap();
        let v1_der = build_synthetic_x509_der(false, &OID_RSA_ENCRYPTION_DER, 0, &pub_der);
        let v1_pem = encode_pem_cert(&v1_der);
        assert!(validate_identity(v1_pem.as_bytes(), key1).is_ok());

        // Non-RSA SPKI OID (ECDSA `id-ecPublicKey` 1.2.840.10045.2.1) is rejected explicitly.
        let ecdsa_oid = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
        let ecdsa_der = build_synthetic_x509_der(true, &ecdsa_oid, 0, &pub_der);
        let err = validate_identity(encode_pem_cert(&ecdsa_der).as_bytes(), key1).unwrap_err();
        assert!(err.to_string().contains("RSA certificate"));

        // Non-byte-aligned BIT STRING (`unused_bits != 0`) and trailing DER garbage fail closed.
        let unaligned_der = build_synthetic_x509_der(true, &OID_RSA_ENCRYPTION_DER, 3, &pub_der);
        let err = validate_identity(encode_pem_cert(&unaligned_der).as_bytes(), key1).unwrap_err();
        assert!(err.to_string().contains("invalid certificate public key"));

        let mut trailing_der = v1_der;
        trailing_der.push(0x00);
        assert!(validate_identity(encode_pem_cert(&trailing_der).as_bytes(), key1).is_err());
    }
}
