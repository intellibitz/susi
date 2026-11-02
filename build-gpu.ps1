# Compatibility entrypoint: native Windows CPU build; no CUDA runtime/toolkit setup.
# CUDA/MKL product backends are retired. Explicit retired features fail in Cargo.
$ErrorActionPreference = "Stop"
cargo xb --release $args
exit $LASTEXITCODE
