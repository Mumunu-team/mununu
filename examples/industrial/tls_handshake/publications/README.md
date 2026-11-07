# Publications — TLS handshake example

Drafts of the two derivative artefacts that publish the results of the TLS handshake example.

## Files

| File | Target | Length |
|---|---|---|
| [`substack.md`](substack.md) | Substack post — technical deep-dive | ~2,700 words |
| [`linkedin.md`](linkedin.md) | LinkedIn post — executive summary | ~190 words |

## Validation gate (per `docs/archive/design/contract-corpus-and-config.md` §D.10)

Before either draft is posted publicly, all four checks must pass. Status is updated by hand when each one is signed off.

- [ ] **Gate 1 — transcript reproduces.** `./examples/industrial/tls_handshake/validate.sh` exits 0 against the pinned commit, and `git diff transcript.txt` is empty.
- [ ] **Gate 2 — transcript matches the post.** Every verdict block quoted in `substack.md` matches `transcript.txt` byte-for-byte for the verdict lines (the commentary around them does not need to match).
- [ ] **Gate 3 — claims integrity signed off.** The author has reviewed `substack.md` against the [CLAUDE.md claims-integrity rules](../../../../CLAUDE.md) and confirmed:
  - No claim mununu found a bug in any real commercial TLS implementation.
  - No severity inflation — the corpus-resolution outcome is described honestly (a vetted contract reference, *not* proof that the AES IP is correct).
  - All abstractions explicitly stated, with the chaotic-stub vs corpus-hit distinction surfaced in the "What this example does not claim" section.
  - The AES corpus entry is described as *illustrative* (matching its `mununu_verified (illustrative)` provenance tag), not a complete formal model of NIST SP 800-38A.
- [ ] **Gate 4 — second reviewer.** A second reviewer (human or `review-orchestrator` agent) has read `substack.md` end-to-end and confirmed that the "What this example does not claim" caveats are not buried below the fold.

Only proceed to posting after all four gates pass. The LinkedIn post can publish only after the Substack post is live (it links to the Substack).

## Sequence for posting

1. Verify all four gates above are checked.
2. Post `substack.md` to the chosen Substack publication. Capture the canonical URL.
3. Replace `[Substack link TBD]` in `linkedin.md` with the canonical URL.
4. Post `linkedin.md`. Capture the canonical URL.
5. Update this README with both canonical URLs (under a new "Published" section).
6. Commit + push the URL updates so the version-controlled drafts point at the live posts.

## Out of scope here

These drafts cover only the TLS handshake example. They are post 3 in the four-document Substack series:

- **Post 1:** Black-box modules in compositional extraction (Document A, `secure_boot_rom`).
- **Post 2:** Two RTL frontends, one IR (Document B, `dual_frontend_soc`).
- **Post 3 (this one):** A contract corpus for hardware verification (Document D, `tls_handshake`).
- **Post 4 (capstone, later):** Formal verification across the HW/SW boundary (Document C).

Each post has its own example directory, its own transcript, and its own validation gate.
