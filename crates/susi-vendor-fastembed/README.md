# Rust-native BGE embeddings

This compatibility facade retains the `susi-vendor-fastembed` package and public
embedding functions while replacing fastembed/ONNX Runtime with Candle BERT.

The default model is `BAAI/bge-small-en-v1.5`, pinned to revision
`5c38ec7c405ec4b44b94cc5a9bb96e735b38267a`. The upstream model card identifies
the model as MIT licensed:
https://huggingface.co/BAAI/bge-small-en-v1.5/blob/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a/README.md

The implementation uses pretrained BERT weights, the upstream tokenizer,
CLS-token pooling and L2 normalization, producing 384-dimensional vectors.
It preserves input order and uses attention-masked batches of at most 32 texts.
Model artifacts use a separate pinned-revision cache directory, so existing
ONNX artifacts are never mistaken for safetensors or deleted.

`FASTEMBED_CACHE_DIR` remains a supported cache-root override.
`SUSI_FASTEMBED_MODEL_URL` accepts only the audited pinned publisher URL.
Other origins or models are refused until they have an artifact-bound license
admission path. Every cached and downloaded config, tokenizer and weight file
is checked against its pinned SHA-256 before use; a custom cache location does
not bypass provenance. No model weights or runtime are downloaded at build time.
Normal tests are hermetic. The ignored real-model qualification test can be
run explicitly with `SUSI_EMBEDDING_TEST_CACHE` pointing at a scratch directory.
It checks dimensions, normalization, semantic ordering and batch/single
consistency. This test alone is not a claim of numerical parity with ONNX.

On 2026-11-01, an explicit side-by-side test ran the previous ONNX facade and
this Candle facade on three texts (two related animal sentences and one database
sentence), using their real pretrained model artifacts in separate scratch
caches. All 384 components were compared for every input. Minimum cosine
similarity was 0.999999999999784 and maximum absolute component difference was
1.3e-7. This is evidence for those samples, not universal numerical equivalence.
A compact ONNX reference prefix is retained in the explicit model test.
