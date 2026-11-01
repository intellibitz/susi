# Rust-native Wasm substrate

The public facade name is retained for source compatibility. The implementation
uses pinned Wasmi 1.0.9 with SIMD and additional runtime checks. The selected
version passes the native Windows regression suite and actual compiled Rust
WASI fixture. Upgrades require the same qualification; a version number alone
is not proof of safe execution.

The independent WASI host grants only:

- argv `[reflex, caller_argument]` and an empty environment;
- empty stdin, bounded captured stdout, bounded sink stderr;
- realtime/monotonic clocks, OS random bytes, yield, and bounded poll/sleep;
- status queries for those three streams and successful zero-status exit.

No filesystem directories, sockets, environment values or host process handles
are inherited. File/socket operations without a capability return explicit
errors. Unsupported operations return NOSYS/NOTCAPABLE rather than pretending
to succeed. This is a deliberately capability-limited host, not a claim of
complete WASI feature conformance.

Guest linear memory remains capped at 256 MiB per declared memory. Each cell or
reflex receives 10 million interpreter fuel units; these units are not equivalent
to the former JIT budget. Stdout plus stderr output is capped at 16 MiB; poll
cannot wait beyond the reflex's five-second wall-time allowance. Guest pointers
and syscall signatures are validated. A module's start section runs before
exported memory is bound; memory-dependent WASI calls there fail rather than
accessing unbound host state. Standard `_start` entrypoints have their memory.

Verification includes existing cell bounds/fuel/host_send tests, argv and stdout
probes, denied filesystem access, malformed-pointer/signature rejection, output
and poll bounds, and an explicitly run `rustc --target wasm32-wasip1` fixture
that checks argv, empty env, denied files, clocks, sleep, stdout and exit.

The previous Wasmtime, Cranelift, WASI-common and wiggle dependency stacks are
not required by this implementation. Parser/encoder/WAT dependencies retain
their upstream licensing choices, including the available Apache-2.0/MIT paths.
