# Susi native TLS boundary

Upstream native-tls 0.2.18, MIT OR Apache-2.0. Original license files retained.

Windows PKCS#8 imports use unpredictable 128-bit OS-random container names and
exclusive creation. An Arc guard retains the owned container through identity,
connector/acceptor, interrupted handshake and live stream lifetimes. Final drop
removes only that container. The narrow deletion ABI follows Microsoft
CryptAcquireContextW CRYPT_DELETEKEYSET semantics. Abrupt process termination
can bypass destructors; crash recovery of orphaned containers remains unimplemented.

Certificate chain import retains the leaf identity instead of returning the last
intermediate. Hostname and trust verification remain native-tls defaults.

Schannel rejected both ephemeral CryptoAPI and CNG key imports in live tests.
Microsoft documents this OS limitation in the SslStream troubleshooting guide:
https://learn.microsoft.com/en-us/dotnet/core/extensions/sslstream-troubleshooting#handshake-failed-with-ephemeral-keys

The Windows PEM importer supports RSA PKCS#8. Existing ECDSA identities are
preserved and rejected explicitly, never overwritten or silently reissued.
Migration requires ownership evidence and retained prior identity. This remains
an acceptance gap. A failed remote TLS setup refuses plaintext regardless of
its configured plaintext preference. Localhost plaintext remains available.

Windows uses Schannel and macOS uses Security.framework. Linux uses dynamically
linked platform OpenSSL; no OpenSSL source is vendored, but Linux is not a pure
Rust TLS implementation and requires separate platform qualification.

Certificate generation uses rcgen without its crypto providers and RustCrypto
RSA 3072-bit signing. RSA is used only for local key generation and a local
self-signature; live handshake cryptography uses Schannel. RustCrypto RSA has an
unresolved Marvin timing advisory and must not be reused for attacker-chosen
network decryption/signing. This is not a certification claim:
https://github.com/RustCrypto/RSA#security-warning

Validation: Windows live local HTTPS with pinned test trust, untrusted issuer
rejection, wrong hostname rejection, malformed PEM rejection, key mismatch
rejection and ALPN negotiation. No OS trust store is modified. The native-tls
owned-container test verifies retention until final owner drop and subsequent
keyset deletion. Its upstream dev dependencies require temporarily including
vendor/native-tls as a workspace member; it is not a production workspace member.
