# Publications — dual-frontend SoC example

Drafts of the two derivative artefacts that publish the results of the Document B (RTL frontend unification) example.

## Files

| File | Target | Length |
|---|---|---|
| [`substack.md`](substack.md) | Substack post — technical deep-dive | ~2,500 words |
| [`linkedin.md`](linkedin.md) | LinkedIn post — executive summary | ~180 words |

## Validation gate (per `docs/archive/design/rtl-frontend-unification.md` §B.9.3)

Before either draft posts publicly, all four checks must pass.

- [ ] **Gate 1 — transcript reproduces.** `./examples/industrial/dual_frontend_soc/validate.sh` exits 0 against the pinned commit, and `git diff transcript.txt` (after the run, ignoring the gitignored `sidecars_generated/` directory) is empty.
- [ ] **Gate 2 — transcript matches the post.** Every verdict block quoted in `substack.md` matches `transcript.txt` byte-for-byte for the verdict lines.
- [ ] **Gate 3 — claims integrity signed off.** No claim of bugs in real silicon; the stand-in role of `mununu contract sidecars` for the yosys auto-emission is named in the post; the contingency on chaotic-stub semantics is named.
- [ ] **Gate 4 — second reviewer.** A second reviewer (human or `review-orchestrator` agent) has read `substack.md` and confirmed the §"What this example does not claim" caveats are not buried.

Posting only proceeds after all four gates pass. The LinkedIn post can publish only after the Substack post is live (it links to the Substack).

## Sequence for posting

1. Verify all four gates above are checked.
2. Post `substack.md` to the chosen Substack publication. Capture the canonical URL.
3. Replace `[Substack link TBD]` in `linkedin.md` with the canonical URL.
4. Post `linkedin.md`. Capture the canonical URL.
5. Update this README with both canonical URLs (under a new "Published" section).
6. Commit + push the URL updates.

## Looking ahead

The capstone publication for the four-document arc is Document C (HW/SW codesign extraction). The current Substack is part 2 of a four-part series; part 3 will be Document D (contract corpus + sidecar), part 4 will be Document C as the capstone.
