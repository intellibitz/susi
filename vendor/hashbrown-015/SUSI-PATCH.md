# Susi hashbrown 0.15 compatibility patch

Upstream hashbrown 0.15.5, Apache-2.0 OR MIT; original notices retained.
The default hasher implementation is ahash 0.8.12 (MIT OR Apache-2.0),
with operating-system runtime random seeding. This replaces foldhash rather
than changing its license metadata. Map/set APIs and storage algorithms are
unchanged. No foldhash implementation is included.

Qualification on native Windows: 100,000 map insertions/lookups, 50,000
removals plus shrink, 10,000 set insertions with duplicate checks passed.
The Susi Wasm facade's 21 ordinary tests passed with this patch selected.
