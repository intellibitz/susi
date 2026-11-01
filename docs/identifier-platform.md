# Table-free Rust identifier classification

The local `unicode-ident` replacement is independently written Apache-2.0 Rust
code. It preserves the two predicate entrypoints used by Syn and proc-macro2,
but contains no upstream Unicode lookup tables, generated character ranges,
downloaded UCD data or redistributed compiler implementation. Its dependency
is the table-free OS adapter described in `unicode-platform.md`.

ASCII uses direct comparisons. Procedural macros validate identifiers through
their already-running compiler using `proc_macro::Ident::new`. Its documented
invalid-input panic is caught without changing panic hooks. TokenStream parsing
is deliberately not used: negative queries can emit diagnostics that poison
an otherwise valid calling compilation.

Outside procedural macros, characters assigned by the operating system use
its XID properties. Exhaustive qualification on Windows Unicode 15.1 found no
XID_Start or XID_Continue differences for OS-assigned scalars against the
previous implementation. Unassigned characters use an already-installed,
concrete Rust 1.99.0 compiler through a tiny, original procedural-macro helper.
This preserves newer Rust identifiers without copying character tables.

One serialized helper handles bounded five-byte requests and one-byte answers.
The queue contains at most 32 requests and the result cache at most 4,096
entries. Startup has ten seconds, a query two seconds, and a caller fifteen
seconds. Ten idle seconds shut down the child and remove its owned files.
Explicit shutdown is also provided. Source-analysis sessions must use it before
short-lived CLI exit. The helper never invokes rustup, installs a toolchain,
consults a project toolchain override, or inherits credentials. Its environment
is cleared except for native compiler/OS discovery inputs.

Missing compiler, saturation, timeout and transport errors are capability
failures, not negative character facts. Checked predicate APIs return these
errors; compatibility predicates fail closed and retain a thread-local error
that Syn's Susi facade must inspect. Infrastructure failures are never cached
as invalid identifiers. `UNICODE_VERSION` is deliberately `(0, 0, 0)` because
no fixed table is shipped; `platform_unicode_version` returns the OS version.
The compiler's accepted repertoire follows the pinned installed toolchain.

Tests cover multilingual identifiers, newer Todhri identifiers, negative
queries without diagnostic poisoning, 10,240 distinct service/cache queries,
concurrent callers, saturation, eviction, idle restart, timeout recovery and
owned-file cleanup. A warm helper was observed at 24,346,624 bytes resident;
this is a sample, not a startup or peak-memory guarantee. Linux/macOS require
their own runtime qualification; Windows results do not certify them.

Primary API contracts: [Rust identifier rules](https://doc.rust-lang.org/reference/identifiers.html)
and [proc_macro::Ident](https://doc.rust-lang.org/proc_macro/struct.Ident.html).
