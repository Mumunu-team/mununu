# Publications — secure boot ROM example

Drafts of the two derivative artefacts that publish the results of the secure boot ROM example.

## Files

| File | Target | Length |
|---|---|---|
| [`substack.md`](substack.md) | Substack post — technical deep-dive | 2,800 words |
| [`linkedin.md`](linkedin.md) | LinkedIn post — executive summary | ~180 words |

## Validation gate (per `docs/archive/design/black-box-modules.md` §10.3)

Before either draft is posted publicly, all four checks must pass. Status is updated by hand when each one is signed off.

- [ ] **Gate 1 — transcript reproduces.** `./examples/industrial/secure_boot_rom/validate.sh` exits 0 against the pinned commit, and `git diff transcript.txt` is empty.
- [ ] **Gate 2 — transcript matches the post.** Every verdict block quoted in `substack.md` matches `transcript.txt` byte-for-byte for the verdict lines (the commentary around them does not need to match).
- [ ] **Gate 3 — claims integrity signed off.** The author has reviewed `substack.md` against the [CLAUDE.md claims-integrity rules](../../../../CLAUDE.md) and confirmed:
  - No claim mununu found a bug in any real commercial product.
  - No severity inflation — the safety property is honestly described as "holds under the contract" not "holds in the real device."
  - All abstractions explicitly stated, with the chaotic-stub vs vendor-contract distinction surfaced in the "What this example does not claim" section.
- [ ] **Gate 4 — second reviewer.** A second reviewer (human or `review-orchestrator` agent) has read `substack.md` end-to-end and confirmed that the §6.iii ("What this example does not claim") caveats are not buried below the fold.

Only proceed to posting after all four gates pass. The LinkedIn post can publish only after the Substack post is live (it links to the Substack).

## Sequence for posting

1. Verify all four gates above are checked.
2. Post `substack.md` to the chosen Substack publication. Capture the canonical URL.
3. Replace `[Substack link TBD]` in `linkedin.md` with the canonical URL.
4. Post `linkedin.md`. Capture the canonical URL.
5. Update this README with both canonical URLs (under a new "Published" section).
6. Commit + push the URL updates so the version-controlled drafts point at the live posts.

## Out of scope here

These drafts cover only the secure boot ROM example. The full Substack series follows the four-document roadmap:

- **Post 1 (this one):** Black-box modules in compositional extraction (Document A).
- **Post 2 (later):** Two RTL frontends, one IR (Document B).
- **Post 3 (later):** A contract corpus for hardware verification (Document D).
- **Post 4 (capstone):** Formal verification across the HW/SW boundary (Document C).

Each post has its own example directory, its own transcript, and its own validation gate.
