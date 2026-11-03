//! RSA key operations use Windows CNG, never a bundled RSA implementation.
#![deny(unsafe_code)]

use pkcs1::der::{Decode, Encode};
use std::io;
use std::ptr;
use std::sync::Mutex;
use windows_sys::Win32::Security::Cryptography::*;
use zeroize::Zeroizing;

const HEADER_BYTES: usize = 24;
// Preserve the former RSA public-key parser's 4096-bit maximum.
const MAX_KEY_BYTES: usize = 512;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn checked_status(status: i32, operation: &str) -> io::Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{operation} failed (NTSTATUS {status:#010x})"
        )))
    }
}

fn byte_count(length: usize) -> io::Result<u32> {
    u32::try_from(length).map_err(|_| invalid("RSA input exceeds Windows API limits"))
}

/// Handle values are held as integers and used only behind the owning key mutex.
/// This avoids an unchecked Send/Sync implementation for a raw pointer. No OS
/// handle is exposed, shared, duplicated or used after the owning value drops.
struct Algorithm(usize);

impl Algorithm {
    #[allow(unsafe_code)] // SAFETY: static terminated algorithm ID, valid output, flags 0.
    fn open(name: windows_sys::core::PCWSTR) -> io::Result<Self> {
        let mut handle = ptr::null_mut();
        let status = unsafe { BCryptOpenAlgorithmProvider(&mut handle, name, ptr::null(), 0) };
        checked_status(status, "BCryptOpenAlgorithmProvider")?;
        if handle.is_null() {
            return Err(invalid("CNG returned an empty algorithm handle"));
        }
        Ok(Self(handle as usize))
    }

    fn handle(&self) -> BCRYPT_ALG_HANDLE {
        self.0 as BCRYPT_ALG_HANDLE
    }
}

impl Drop for Algorithm {
    #[allow(unsafe_code)] // SAFETY: this exclusively owned successful provider handle is closed once.
    fn drop(&mut self) {
        unsafe { BCryptCloseAlgorithmProvider(self.handle(), 0) };
    }
}

struct Key {
    handle: usize,
    // Retain the provider until after the key's Drop destroys its handle.
    _algorithm: Algorithm,
}

impl Key {
    fn handle(&self) -> BCRYPT_KEY_HANDLE {
        self.handle as BCRYPT_KEY_HANDLE
    }

    #[allow(unsafe_code)] // SAFETY: owned live provider, valid output; generated key is immediately owned.
    fn generate() -> io::Result<Self> {
        let algorithm = Algorithm::open(BCRYPT_RSA_ALGORITHM)?;
        let mut handle = ptr::null_mut();
        checked_status(
            unsafe { BCryptGenerateKeyPair(algorithm.handle(), &mut handle, 3072, 0) },
            "BCryptGenerateKeyPair",
        )?;
        if handle.is_null() {
            return Err(invalid("CNG returned an empty RSA key handle"));
        }
        let key = Self {
            handle: handle as usize,
            _algorithm: algorithm,
        };
        // The owned key destroys the handle if finalization fails.
        checked_status(
            unsafe { BCryptFinalizeKeyPair(key.handle(), 0) },
            "BCryptFinalizeKeyPair",
        )?;
        Ok(key)
    }

    #[allow(unsafe_code)] // SAFETY: borrowed bounded blob stays live; output becomes exclusively owned.
    fn import(blob: &[u8]) -> io::Result<Self> {
        let algorithm = Algorithm::open(BCRYPT_RSA_ALGORITHM)?;
        let mut handle = ptr::null_mut();
        checked_status(
            unsafe {
                // Flags 0 preserves CNG key validation. Never use BCRYPT_NO_KEY_VALIDATION.
                BCryptImportKeyPair(
                    algorithm.handle(),
                    ptr::null_mut(),
                    BCRYPT_RSAFULLPRIVATE_BLOB,
                    &mut handle,
                    blob.as_ptr(),
                    byte_count(blob.len())?,
                    0,
                )
            },
            "BCryptImportKeyPair",
        )?;
        if handle.is_null() {
            return Err(invalid("CNG returned an empty imported RSA key handle"));
        }
        Ok(Self {
            handle: handle as usize,
            _algorithm: algorithm,
        })
    }

    #[allow(unsafe_code)] // SAFETY: locked live key; CNG first sizes then writes a bounded owned buffer.
    fn export(&self, private: bool) -> io::Result<Zeroizing<Vec<u8>>> {
        let format = if private {
            BCRYPT_RSAFULLPRIVATE_BLOB
        } else {
            BCRYPT_RSAPUBLIC_BLOB
        };
        let mut size = 0;
        checked_status(
            unsafe {
                BCryptExportKey(
                    self.handle(),
                    ptr::null_mut(),
                    format,
                    ptr::null_mut(),
                    0,
                    &mut size,
                    0,
                )
            },
            "BCryptExportKey(size)",
        )?;
        if size as usize > 9 * MAX_KEY_BYTES + HEADER_BYTES || (size as usize) < HEADER_BYTES {
            return Err(invalid("CNG returned an unsupported RSA blob size"));
        }
        let mut bytes = Zeroizing::new(vec![0; size as usize]);
        let capacity = size;
        checked_status(
            unsafe {
                BCryptExportKey(
                    self.handle(),
                    ptr::null_mut(),
                    format,
                    bytes.as_mut_ptr(),
                    capacity,
                    &mut size,
                    0,
                )
            },
            "BCryptExportKey",
        )?;
        if size > capacity {
            return Err(invalid("CNG returned an invalid RSA blob length"));
        }
        bytes.truncate(size as usize);
        Ok(bytes)
    }
}

impl Drop for Key {
    #[allow(unsafe_code)] // SAFETY: exclusively owned successful key handle is destroyed exactly once.
    fn drop(&mut self) {
        unsafe { BCryptDestroyKey(self.handle()) };
    }
}

pub(crate) struct PlatformRsaKey {
    key: Mutex<Key>,
}

impl PlatformRsaKey {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            key: Mutex::new(Key::generate()?),
        })
    }

    pub(crate) fn from_pkcs8(pem: &[u8]) -> io::Result<Self> {
        let pem = std::str::from_utf8(pem).map_err(|_| invalid("invalid private key PEM"))?;
        let (label, document) = pkcs8::SecretDocument::from_pem(pem)
            .map_err(|_| invalid("invalid PKCS#8 private key"))?;
        if label != "PRIVATE KEY" {
            return Err(invalid("expected an unencrypted PKCS#8 private key"));
        }
        let info = pkcs8::PrivateKeyInfo::from_der(document.as_bytes())
            .map_err(|_| invalid("invalid PKCS#8 private key"))?;
        if info.algorithm.oid != pkcs1::ALGORITHM_OID
            || info.algorithm.parameters != Some(pkcs1::der::asn1::AnyRef::NULL)
        {
            return Err(invalid(
                "identity requires RSA encryption PKCS#8 parameters",
            ));
        }
        let private = pkcs1::RsaPrivateKey::from_der(info.private_key)
            .map_err(|_| invalid("invalid PKCS#1 private key"))?;
        if private.other_prime_infos.is_some() {
            return Err(invalid("multi-prime RSA identities are unsupported"));
        }
        let blob = private_blob(&private)?;
        validate_private_components(&private)?;
        Ok(Self {
            key: Mutex::new(Key::import(&blob)?),
        })
    }

    pub(crate) fn public_key_der(&self) -> io::Result<Vec<u8>> {
        let key = self
            .key
            .lock()
            .map_err(|_| io::Error::other("RSA key lock poisoned"))?;
        let blob = key.export(false)?;
        let fields = BlobFields::parse(&blob, false)?;
        pkcs1::RsaPublicKey {
            modulus: uint(fields.modulus)?,
            public_exponent: uint(fields.exponent)?,
        }
        .to_der()
        .map_err(|_| invalid("could not encode RSA public key"))
    }

    pub(crate) fn private_key_pem(&self) -> io::Result<Vec<u8>> {
        let key = self
            .key
            .lock()
            .map_err(|_| io::Error::other("RSA key lock poisoned"))?;
        let blob = key.export(true)?;
        let fields = BlobFields::parse(&blob, true)?;
        let private = pkcs1::RsaPrivateKey {
            modulus: uint(fields.modulus)?,
            public_exponent: uint(fields.exponent)?,
            private_exponent: uint(fields.private_exponent)?,
            prime1: uint(fields.prime1)?,
            prime2: uint(fields.prime2)?,
            exponent1: uint(fields.exponent1)?,
            exponent2: uint(fields.exponent2)?,
            coefficient: uint(fields.coefficient)?,
            other_prime_infos: None,
        };
        let der = Zeroizing::new(
            private
                .to_der()
                .map_err(|_| invalid("could not encode RSA private key"))?,
        );
        let info = pkcs8::PrivateKeyInfo::new(pkcs1::ALGORITHM_ID, &der);
        let document: pkcs8::SecretDocument = info
            .try_into()
            .map_err(|_| invalid("could not encode PKCS#8 private key"))?;
        let pem = document
            .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
            .map_err(|_| invalid("could not encode private key PEM"))?;
        Ok(pem.as_bytes().to_vec())
    }

    #[allow(unsafe_code)] // SAFETY: key mutex serializes its live handle; hash/padding/output are bounded and live.
    pub(crate) fn sign_sha256(&self, message: &[u8]) -> io::Result<Vec<u8>> {
        let digest = sha256(message)?;
        let key = self
            .key
            .lock()
            .map_err(|_| io::Error::other("RSA key lock poisoned"))?;
        let padding = BCRYPT_PKCS1_PADDING_INFO {
            pszAlgId: BCRYPT_SHA256_ALGORITHM,
        };
        let mut size = 0;
        checked_status(
            unsafe {
                BCryptSignHash(
                    key.handle(),
                    (&padding as *const BCRYPT_PKCS1_PADDING_INFO).cast(),
                    digest.as_ptr(),
                    32,
                    ptr::null_mut(),
                    0,
                    &mut size,
                    BCRYPT_PAD_PKCS1,
                )
            },
            "BCryptSignHash(size)",
        )?;
        if size == 0 || size as usize > MAX_KEY_BYTES {
            return Err(invalid("CNG returned an unsupported signature size"));
        }
        let capacity = size;
        let mut signature = vec![0; capacity as usize];
        checked_status(
            unsafe {
                BCryptSignHash(
                    key.handle(),
                    (&padding as *const BCRYPT_PKCS1_PADDING_INFO).cast(),
                    digest.as_ptr(),
                    32,
                    signature.as_mut_ptr(),
                    capacity,
                    &mut size,
                    BCRYPT_PAD_PKCS1,
                )
            },
            "BCryptSignHash",
        )?;
        if size > capacity {
            return Err(invalid("CNG returned an invalid signature length"));
        }
        signature.truncate(size as usize);
        Ok(signature)
    }

    #[allow(unsafe_code)] // SAFETY: key mutex serializes live handle; borrowed padding/digest/signature remain live.
    pub(crate) fn verify_sha256(&self, message: &[u8], signature: &[u8]) -> io::Result<()> {
        let digest = sha256(message)?;
        let key = self
            .key
            .lock()
            .map_err(|_| io::Error::other("RSA key lock poisoned"))?;
        let padding = BCRYPT_PKCS1_PADDING_INFO {
            pszAlgId: BCRYPT_SHA256_ALGORITHM,
        };
        checked_status(
            unsafe {
                BCryptVerifySignature(
                    key.handle(),
                    (&padding as *const BCRYPT_PKCS1_PADDING_INFO).cast(),
                    digest.as_ptr(),
                    32,
                    signature.as_ptr(),
                    byte_count(signature.len())?,
                    BCRYPT_PAD_PKCS1,
                )
            },
            "BCryptVerifySignature",
        )
    }
}

#[allow(unsafe_code)] // SAFETY: live SHA256 provider, checked borrowed input length and exact 32-byte output.
fn sha256(input: &[u8]) -> io::Result<[u8; 32]> {
    let algorithm = Algorithm::open(BCRYPT_SHA256_ALGORITHM)?;
    let mut digest = [0; 32];
    checked_status(
        unsafe {
            BCryptHash(
                algorithm.handle(),
                ptr::null(),
                0,
                input.as_ptr(),
                byte_count(input.len())?,
                digest.as_mut_ptr(),
                32,
            )
        },
        "BCryptHash(SHA256)",
    )?;
    Ok(digest)
}

fn uint(bytes: &[u8]) -> io::Result<pkcs1::UintRef<'_>> {
    pkcs1::UintRef::new(bytes).map_err(|_| invalid("invalid RSA integer"))
}

fn push_fixed(blob: &mut Vec<u8>, bytes: &[u8], width: usize) -> io::Result<()> {
    if bytes.len() > width || width == 0 || width > MAX_KEY_BYTES {
        return Err(invalid("RSA component has an unsupported width"));
    }
    blob.resize(blob.len() + width - bytes.len(), 0);
    blob.extend_from_slice(bytes);
    Ok(())
}

const LIMBS: usize = MAX_KEY_BYTES / 8;
type Limbs = [u64; LIMBS];
type Wide = [u64; LIMBS * 2];
const ZERO: Limbs = [0u64; LIMBS];

fn cmp_limbs(a: &Limbs, b: &Limbs) -> std::cmp::Ordering {
    a.iter().rev().cmp(b.iter().rev())
}

fn integer(bytes: &[u8]) -> io::Result<Zeroizing<Limbs>> {
    let mut fixed = Zeroizing::new([0u8; MAX_KEY_BYTES]);
    let first = fixed
        .len()
        .checked_sub(bytes.len())
        .ok_or_else(|| invalid("oversized RSA integer"))?;
    fixed[first..].copy_from_slice(bytes);
    let mut out = Zeroizing::new(ZERO);
    for (limb, chunk) in out.iter_mut().zip(fixed.rchunks_exact(8)) {
        let word: [u8; 8] = chunk
            .try_into()
            .map_err(|_| invalid("invalid RSA integer limb"))?;
        *limb = u64::from_be_bytes(word);
    }
    Ok(out)
}

fn sub_one(a: &Limbs) -> Zeroizing<Limbs> {
    let mut out = Zeroizing::new(*a);
    for limb in out.iter_mut() {
        let (next, borrow) = limb.overflowing_sub(1);
        *limb = next;
        if !borrow {
            break;
        }
    }
    out
}

fn mul_wide(a: &Limbs, b: &Limbs) -> Zeroizing<Wide> {
    let mut out = Zeroizing::new([0u64; LIMBS * 2]);
    for i in 0..LIMBS {
        let mut carry = 0u128;
        for j in 0..LIMBS {
            let prod = u128::from(a[i]) * u128::from(b[j]) + u128::from(out[i + j]) + carry;
            out[i + j] = prod as u64;
            carry = prod >> 64;
        }
        out[i + LIMBS] = carry as u64;
    }
    out
}

fn rem_wide(dividend: &Wide, divisor: &Limbs) -> io::Result<Zeroizing<Limbs>> {
    if *divisor == ZERO {
        return Err(invalid("zero RSA divisor"));
    }
    let Some(top_limb) = dividend.iter().rposition(|&limb| limb != 0) else {
        return Ok(Zeroizing::new(ZERO));
    };
    let mut rem = Zeroizing::new([0u64; LIMBS + 1]);
    for word in (0..=top_limb).rev() {
        let value = dividend[word];
        for shift in (0..64).rev() {
            let mut carry = (value >> shift) & 1;
            for limb in rem.iter_mut() {
                let next = (*limb << 1) | carry;
                carry = *limb >> 63;
                *limb = next;
            }
            if rem[LIMBS] != 0
                || rem[..LIMBS].iter().rev().cmp(divisor.iter().rev()) != std::cmp::Ordering::Less
            {
                let mut borrow = 0u64;
                for (r, &d) in rem[..LIMBS].iter_mut().zip(divisor.iter()) {
                    let (diff1, b1) = r.overflowing_sub(d);
                    let (diff2, b2) = diff1.overflowing_sub(borrow);
                    *r = diff2;
                    borrow = u64::from(b1 || b2);
                }
                rem[LIMBS] = rem[LIMBS].wrapping_sub(borrow);
            }
        }
    }
    let mut out = Zeroizing::new(ZERO);
    out.copy_from_slice(&rem[..LIMBS]);
    Ok(out)
}

fn product_is_one(a: &Limbs, b: &Limbs, modulus: &Limbs) -> io::Result<bool> {
    let mut one = ZERO;
    one[0] = 1;
    let wide = mul_wide(a, b);
    let reduced = rem_wide(&wide, modulus)?;
    Ok(*reduced == one)
}

// CNG can reconstruct ignored CRT fields during import. Check the supplied
// PKCS#1 integers before it does. In particular a valid d can be reduced
// modulo either phi(n) or lambda(n), so equality with an exported d is not
// a validity test. Only installed CNG performs signing and key generation.
fn validate_private_components(private: &pkcs1::RsaPrivateKey<'_>) -> io::Result<()> {
    let mut one = ZERO;
    one[0] = 1;
    let n = integer(private.modulus.as_bytes())?;
    let e = integer(private.public_exponent.as_bytes())?;
    let d = integer(private.private_exponent.as_bytes())?;
    let p = integer(private.prime1.as_bytes())?;
    let q = integer(private.prime2.as_bytes())?;
    let dp = integer(private.exponent1.as_bytes())?;
    let dq = integer(private.exponent2.as_bytes())?;
    let coefficient = integer(private.coefficient.as_bytes())?;
    if cmp_limbs(&p, &one) != std::cmp::Ordering::Greater
        || cmp_limbs(&q, &one) != std::cmp::Ordering::Greater
        || *p == *q
        || *d == ZERO
        || cmp_limbs(&d, &n) != std::cmp::Ordering::Less
        || cmp_limbs(&coefficient, &p) != std::cmp::Ordering::Less
    {
        return Err(invalid("invalid RSA private components"));
    }
    let product = mul_wide(&p, &q);
    let pm = sub_one(&p);
    let qm = sub_one(&q);
    let mut d_wide = Zeroizing::new([0u64; LIMBS * 2]);
    d_wide[..LIMBS].copy_from_slice(&*d);
    let d_mod_p = rem_wide(&d_wide, &pm)?;
    let d_mod_q = rem_wide(&d_wide, &qm)?;
    if product[..LIMBS] != n[..]
        || product[LIMBS..] != ZERO[..]
        || *dp != *d_mod_p
        || *dq != *d_mod_q
        || !product_is_one(&e, &dp, &pm)?
        || !product_is_one(&e, &dq, &qm)?
        || !product_is_one(&q, &coefficient, &p)?
    {
        return Err(invalid("inconsistent RSA private components"));
    }
    Ok(())
}

fn private_blob(private: &pkcs1::RsaPrivateKey<'_>) -> io::Result<Zeroizing<Vec<u8>>> {
    let modulus = private.modulus.as_bytes();
    let first = modulus
        .first()
        .copied()
        .ok_or_else(|| invalid("empty RSA modulus"))?;
    let bit_length = modulus
        .len()
        .checked_mul(8)
        .and_then(|bits| bits.checked_sub(first.leading_zeros() as usize))
        .ok_or_else(|| invalid("invalid RSA modulus length"))?;
    if !(512..=MAX_KEY_BYTES * 8).contains(&bit_length) || bit_length % 8 != 0 {
        return Err(invalid(
            "RSA identity must have a 512-4096-bit byte-aligned modulus",
        ));
    }
    let exponent = private.public_exponent.as_bytes();
    if exponent.is_empty() || exponent.len() > 8 {
        return Err(invalid("unsupported RSA public exponent"));
    }
    let p_len = private.prime1.as_bytes().len();
    let q_len = private.prime2.as_bytes().len();
    if p_len == 0 || q_len == 0 || p_len > modulus.len() || q_len > modulus.len() {
        return Err(invalid("invalid RSA prime widths"));
    }
    // All component widths above are bounded by the 512-byte modulus.
    // Reserve the entire blob before writing secrets so no old allocation
    // containing private components can escape Zeroizing on reallocation.
    let capacity = HEADER_BYTES + exponent.len() + 2 * modulus.len() + 3 * p_len + 2 * q_len;
    let mut blob = Zeroizing::new(Vec::with_capacity(capacity));
    for value in [
        BCRYPT_RSAFULLPRIVATE_MAGIC,
        byte_count(bit_length)?,
        byte_count(exponent.len())?,
        byte_count(modulus.len())?,
        byte_count(p_len)?,
        byte_count(q_len)?,
    ] {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob.extend_from_slice(exponent);
    blob.extend_from_slice(modulus);
    push_fixed(&mut blob, private.prime1.as_bytes(), p_len)?;
    push_fixed(&mut blob, private.prime2.as_bytes(), q_len)?;
    push_fixed(&mut blob, private.exponent1.as_bytes(), p_len)?;
    push_fixed(&mut blob, private.exponent2.as_bytes(), q_len)?;
    push_fixed(&mut blob, private.coefficient.as_bytes(), p_len)?;
    push_fixed(
        &mut blob,
        private.private_exponent.as_bytes(),
        modulus.len(),
    )?;
    Ok(blob)
}

struct BlobFields<'a> {
    exponent: &'a [u8],
    modulus: &'a [u8],
    prime1: &'a [u8],
    prime2: &'a [u8],
    exponent1: &'a [u8],
    exponent2: &'a [u8],
    coefficient: &'a [u8],
    private_exponent: &'a [u8],
}

impl<'a> BlobFields<'a> {
    fn parse(blob: &'a [u8], private: bool) -> io::Result<Self> {
        let mut remaining = blob;
        let mut words = [0u32; 6];
        for value in &mut words {
            let bytes: [u8; 4] = take(&mut remaining, 4)?
                .try_into()
                .map_err(|_| invalid("truncated RSA blob header"))?;
            *value = u32::from_le_bytes(bytes);
        }
        let expected_magic = if private {
            BCRYPT_RSAFULLPRIVATE_MAGIC
        } else {
            BCRYPT_RSAPUBLIC_MAGIC
        };
        if words[0] != expected_magic
            || words[2] == 0
            || words[2] > 8
            || words[3] == 0
            || words[3] as usize > MAX_KEY_BYTES
            || words[1] != words[3] * 8
        {
            return Err(invalid("invalid RSA CNG blob header"));
        }
        let exponent = take(&mut remaining, words[2] as usize)?;
        let modulus = take(&mut remaining, words[3] as usize)?;
        let mut fields = Self {
            exponent,
            modulus,
            prime1: &[],
            prime2: &[],
            exponent1: &[],
            exponent2: &[],
            coefficient: &[],
            private_exponent: &[],
        };
        if private {
            if words[4] == 0 || words[5] == 0 || words[4] > words[3] || words[5] > words[3] {
                return Err(invalid("invalid RSA CNG prime lengths"));
            }
            fields.prime1 = take(&mut remaining, words[4] as usize)?;
            fields.prime2 = take(&mut remaining, words[5] as usize)?;
            fields.exponent1 = take(&mut remaining, words[4] as usize)?;
            fields.exponent2 = take(&mut remaining, words[5] as usize)?;
            fields.coefficient = take(&mut remaining, words[4] as usize)?;
            fields.private_exponent = take(&mut remaining, words[3] as usize)?;
        } else if words[4] != 0 || words[5] != 0 {
            return Err(invalid(
                "public RSA blob includes private component lengths",
            ));
        }
        if !remaining.is_empty() {
            return Err(invalid("unexpected bytes after RSA CNG blob"));
        }
        Ok(fields)
    }
}

fn take<'a>(remaining: &mut &'a [u8], length: usize) -> io::Result<&'a [u8]> {
    if length > remaining.len() {
        return Err(invalid("truncated RSA CNG blob"));
    }
    let (field, rest) = remaining.split_at(length);
    *remaining = rest;
    Ok(field)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test failures identify the exact failed platform operation.
mod tests {
    use super::*;

    #[test]
    fn cng_rsa_pkcs8_roundtrip_signs_and_rejects_wrong_message() {
        let original = PlatformRsaKey::new().unwrap();
        let pem = Zeroizing::new(original.private_key_pem().unwrap());
        let imported = PlatformRsaKey::from_pkcs8(&pem).unwrap();
        assert_eq!(
            original.public_key_der().unwrap(),
            imported.public_key_der().unwrap()
        );
        let signature = imported.sign_sha256(b"Susi platform RSA proof").unwrap();
        original
            .verify_sha256(b"Susi platform RSA proof", &signature)
            .unwrap();
        assert!(original
            .verify_sha256(b"different message", &signature)
            .is_err());
    }

    #[test]
    fn cng_rsa_rejects_corrupt_private_exponent_with_same_public_key() {
        let original = PlatformRsaKey::new().unwrap();
        let pem = Zeroizing::new(original.private_key_pem().unwrap());
        let (_, document) =
            pkcs8::SecretDocument::from_pem(std::str::from_utf8(&pem).unwrap()).unwrap();
        let info = pkcs8::PrivateKeyInfo::from_der(document.as_bytes()).unwrap();
        let mut private = pkcs1::RsaPrivateKey::from_der(info.private_key).unwrap();
        private.private_exponent = pkcs1::UintRef::new(&[1]).unwrap();
        let der = Zeroizing::new(private.to_der().unwrap());
        let altered: pkcs8::SecretDocument = pkcs8::PrivateKeyInfo::new(pkcs1::ALGORITHM_ID, &der)
            .try_into()
            .unwrap();
        let altered = altered
            .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
            .unwrap();
        assert!(PlatformRsaKey::from_pkcs8(altered.as_bytes()).is_err());
    }

    #[test]
    fn cng_blob_reader_rejects_truncation_and_excess() {
        assert!(BlobFields::parse(&[], false).is_err());
        let original = PlatformRsaKey::new().unwrap();
        let mut blob = original.key.lock().unwrap().export(false).unwrap();
        blob.push(0);
        assert!(BlobFields::parse(&blob, false).is_err());
    }
}
