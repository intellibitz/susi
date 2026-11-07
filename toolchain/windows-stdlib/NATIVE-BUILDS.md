# Native product standard library and release checks

The historical directory name is retained for existing Windows automation.
The Rust builder and verifier select the actual native compiler host. They
support x86_64 Windows MSVC and GNU/Linux x86_64 and aarch64, with Rust 1.99.0
commit b940084d7eb6a299eb4bfeb8e34901bc051e7ac4. Cross compilation, musl and
other target contracts fail before qualification. macOS needs its own verified
standard-library and Mach-O artifact path; it is not silently admitted.

Windows uses the existing OS ICU ABI and Unicode 15.1 contract. Linux does not
link ICU. At the first Unicode operation it loads the newest qualified OS ICU
present in a standard absolute system library directory (the same directory
policy as the vendored IDNA and regex adapters) and resolves its `_<major>`
C symbols. The qualified Linux contracts are ICU 76 with Unicode 16.0 (Debian 13), ICU 74
with Unicode 15.1 (Ubuntu 24.04 LTS), and ICU 72 with Unicode 15.0 (Debian 12
Bookworm). The selected library must report exactly its recorded Unicode
version; a host with none stops with a message naming the qualified libraries.
On Linux `char::UNICODE_VERSION` reports 15.0.0, the oldest qualified
repertoire (Windows reports 15.1.0). No ICU code or Unicode tables are copied into the
distribution, and shipped executables carry no ICU DT_NEEDED entry. The
consumer test reads `/proc/self/maps` to identify the ICU this process loaded
and verifies its repertoire, normalization-sensitive case conversion,
threading and unwinding. Other ICU majors require adding a contract to
`linux_icu::QUALIFIED` and running the consumer against that library.

Prerequisites are the pinned compiler with rust-src, a C compiler driver and
LLD, system ICU 76, 74, or 72, and system OpenSSL 3 development headers/shared libraries.
On x86_64 Rust supplies LLD; aarch64 needs a native installed LLD. Dependency
provisioning happens before the offline native build. Retain toolchain/package
caches in the cloud snapshot; do not rebuild the full workspace in install or
publication scripts.

`cargo local-release prepare-sysroot` authenticates the upstream sources,
executes the tracked builder, inventories target archives, and runs the consumer
with ordinary stable rustc. `cargo local-build debug` uses that sysroot for an
isolated development build. Neither command qualifies or promotes production.
Linux archive handles and pre/post hashes detect mutation; unlike Windows read
handles, they do not provide mandatory write exclusion.

The Linux OpenSSL exception is limited to the reviewed Rust openssl-sys 0.9.117
binding with no features, exact manifest and build-source hashes. Its C input is
only preprocessed for header constants, not compiled into a shipped object.
Build commands select installed system libraries and dynamic linkage, clearing
inherited OpenSSL and loader overrides. Release qualification additionally
requires ELF target/interpreter/DT_NEEDED inspection and a bounded LLD link map
for every shipped executable. Unknown libraries, RPATH/RUNPATH, foreign static
archives, and unreviewed raw objects fail qualification. Map text and hashes are
retained in artifact-bound release evidence. Inventory success alone cannot
qualify a release. Existing Windows PE/link-map gates remain in force.

Validation performed on GNU/Linux x86_64 is recorded in the task evidence;
aarch64 and Windows execution must be validated on their native hosts. The full
combined-tree integration, licenses, lint/tests, smoke/inference, promotion and
rollback gates still govern every production release.

Dependent agent changes can be integrated together with `cargo local-workflow
integrate <name> [name ...]`. Each source retains its scope guard; the merged
tree runs the full gates once, and all receipts are published in one Git
transaction. Failed verification keeps every reservation and candidate.
