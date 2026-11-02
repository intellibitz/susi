# Public test-only RSA identity

`accepted-rsa-cert.pem` and `accepted-rsa-key.pem` were generated solely for this
compatibility regression by the unchanged `susi-vendor-rcgen` implementation at
accepted commit `aff638d06825f0874231ff01b546f9128a534d04`. That implementation used
RustCrypto RSA 0.9.10, RSA-3072, PKCS#8 PEM and a localhost self-signed certificate.
The generation probe and receipts are retained outside the source checkout.

This intentionally public test private key is not a user identity or credential.
Never install it, add it to an OS trust store or use it outside isolated tests.
The regression proves an identity produced before the platform-provider repair
still imports and completes a local TLS handshake without regenerating its key.
