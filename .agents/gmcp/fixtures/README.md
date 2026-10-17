# GMCP fixtures

Deterministic positive and negative protocol fixtures for the VC-203
requirement tests. Convention (see `docs/gmcp/contract-203-001.md`):

- `positive/` holds exchanges that a conformant GMCP endpoint MUST accept or
  produce; `negative/` holds inputs a conformant endpoint MUST reject, plus
  tampered ledger/manifest artifacts (`manifest-*`, `requirement-row-*`) the
  VC-203-001 verifier must fail closed on — exercised by
  `gmcp_req_203_001_04_adversarial`.
- Every fixture is a single JSON object with `comment` (which ledger row it
  exercises), `request`, and `expect` (`result` or `error`).
- `divergences.json` records fixture replays whose spec-expected outcome
  diverges from observed endpoint behavior (the retained regression record);
  `gmcp_req_203_001_05_interop` replays every entry verbatim and fails if the
  recorded observation drifts.
- Fixtures are immutable once referenced by a ledger row or an evidence
  record: corrections land as new files, never edits.
