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
`SUSI_FASTEMBED_MODEL_URL`, when explicitly set, must now expose compatible
`config.json`, `tokenizer.json`, and `model.safetensors` files; an ONNX-only
endpoint is no longer suitable. Overrides must satisfy the same model-license
policy. No model weights or third-party runtime are downloaded at build time.

Normal tests are hermetic. The ignored real-model qualification test can be
run explicitly with `SUSI_EMBEDDING_TEST_CACHE` pointing at a scratch directory.
It checks dimensions, normalization, semantic ordering and batch/single
consistency. This test alone is not a claim of numerical parity with ONNX.
