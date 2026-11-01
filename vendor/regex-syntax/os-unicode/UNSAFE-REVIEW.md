# OS Unicode unsafe boundary review

The crate denies unsafe code by default. Exceptions cover individual OS/ICU
call statements, exact foreign declarations, loader ownership transfer and
symbol conversion only. No module has an unsafe exemption.

The loader owns one reference; symbols borrow that owner. Send/Sync cover the
OS loader handle, not arbitrary function signatures. Windows loading is
restricted to System32. Dynamic symbol conversion checks pointer size and
requires the caller's exact documented ICU signature. ICU sets are checked
for null, owned until Drop, and queried using bounded buffers and range counts.
Returned codepoints are checked and surrogate endpoints normalized. Version
and character-age calls receive four-byte writable arrays. Runtime caches are
immutable after OnceLock initialization and never serialized as shipped data.

Actual command: `cargo geiger --manifest-path <absolute backend Cargo.toml>
--target x86_64-pc-windows-msvc --all-dependencies --offline --output-format Json`.
The companion receipt contains actual scanner counts and hashes of the scanned
source. No unsafe baseline was changed. This is the isolated backend boundary
review; combined release graph evidence remains separately required.
