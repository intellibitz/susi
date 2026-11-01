# Susi fluent-uri storage patch

Based on fluent-uri 0.4.1 (MIT); upstream LICENSE and all parser, normalizer,
resolver, serializer and test code are retained.

Replaces the external borrow-or-share container abstraction with a locally
implemented text-only storage module. fluent-uri seals its URI Value to &str
and String; the module therefore implements those two storage forms only.
It preserves long-lived component references for borrowed input and ties
owned input references to their owner, with no unsafe code and no copies.
The external crate and its feature dependency are removed, not relabeled.

Qualification on native Windows: all 17 upstream integration tests and 95
upstream documentation tests pass; two additional lifetime/international-text
tests pass. The no-default-features/no-alloc build and downstream referencing
0.56 compilation pass.
