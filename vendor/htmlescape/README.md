# Internal snippet encoder

This independently written Apache-2.0 implementation provides the sole API
used by Susi's Tantivy snippets: `htmlescape::encode_minimal(&str) -> String`.
It escapes double quotes, ampersands, apostrophes, less-than and greater-than
characters, preserving all other Unicode scalars. Existing entity text is
escaped as ordinary input; this function does not decode or normalize input.

The package name and version enable a local Cargo patch for that private
consumer. This is not a general upstream API replacement and is not published.
There are no third-party dependencies, copied upstream implementation, or
character tables. Differential verification uses the old registry source only
as an external test reference; that reference is not part of this package.
