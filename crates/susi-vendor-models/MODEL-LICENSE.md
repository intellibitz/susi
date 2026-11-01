# Model-weight admission

Susi's native installer resolves the publisher's Hugging Face revision and
model-card license before transferring weights. Apache-2.0, MIT, BSD-2-Clause
and BSD-3-Clause declarations are accepted; absent, mixed and other declarations
are refused. Every declared base-model repository is checked as well, bounded
to sixteen publishers. These records are publisher declarations, not a claim
that Susi independently establishes model-training intellectual-property rights.

Weight downloads use an immutable revision and the publisher's LFS SHA-256.
Small tokenizer/config companions without LFS hashes are fetched from that
same immutable revision with an 8 MiB cap and hashed. Publication records a
license receipt bound to actual bytes; native load verifies the receipt and
content. Old installs with source provenance, and the known default model,
can acquire a receipt automatically after matching publisher bytes. Unknown
user files are preserved and remain discoverable but are not loaded without
publisher provenance. No manual policy configuration is required.

The default Qwen2.5-0.5B-Instruct-GGUF and declared base model use Apache-2.0.
The embedding facade independently pins the MIT BGE model revision and hashes
of all three artifacts; a cache or URL override cannot substitute other bytes.

Ollama-managed pulls are refused before transfer until a registry manifest and
license-layer verifier can bind admission to immutable artifact digests. An
unrelated Hugging Face model card cannot establish the license of an Ollama tag.
Discovery and inference through explicitly external services remain available;
proprietary remote inference does not distribute model weights through Susi.

Qualification includes offline hostile URL, unsupported license, malformed
lineage, missing receipt and changed-content tests; an explicit ignored test
checks live default-model publisher metadata without downloading weights.

License sidecars are not a trust authority. Publication requires publisher
admission performed in the current process. A cold load of an arbitrary
model reauthenticates the immutable publisher metadata, subject to the user's
network-egress policy. A process-local authenticated cache avoids repeated
metadata requests. Only the audited starter's exact GGUF and tokenizer hashes
and full declared lineage are anchored in the executable for offline cold
loads. A protected persistent trust store for other models remains future
work; this limitation is explicit rather than accepting caller-written claims.
Unknown existing tokenizer files are never truncated or replaced by admission.
Small companion files additionally match the authenticated publisher Git blob
identity and their metadata fetch does not follow redirects.
Because publisher cards declare ancestor repositories without immutable ancestor
revisions, cold reauthentication can fail safely when an ancestor's main revision
has moved since admission. Susi currently refuses that stale receipt rather
than treating changed declarations as identical evidence. Supporting durable
custom offline trust and revision-aware lineage migration requires a separately
reviewed extension; neither is claimed by this patch.
