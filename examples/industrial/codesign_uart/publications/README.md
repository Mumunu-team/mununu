# Publications — codesign UART example

Drafts for the M4.d derivative artefacts that publish the results of the codesign UART example.

## Files

| File | Target | Length |
|---|---|---|
| [`substack.md`](substack.md) | Substack post — example-led narrative | ~2,400 words |
| [`linkedin.md`](linkedin.md) | LinkedIn post — executive summary | ~190 words |

## Editorial framing rule

Both drafts follow the **example-led framing rule** captured in user feedback: lead with the real industrial problem, walk the worked example end-to-end, and surface new mununu capabilities only where the example actually exercises them. Feature catalogues belong in closing paragraphs, not headlines.

## Validation gate (per `docs/archive/design/hw-sw-codesign-extraction.md` §C.11.3)

Before either draft is posted publicly, all four checks must pass.

- [ ] **Gate 1 — transcript reproduces.** `./examples/industrial/codesign_uart/validate.sh` exits 0; `git diff transcript.txt` is empty.
- [ ] **Gate 2 — transcript matches the post.** Every verdict block quoted in `substack.md` matches `transcript.txt` byte-for-byte for the verdict lines.
- [ ] **Gate 3 — claims integrity signed off** (per [CLAUDE.md](../../../../CLAUDE.md)):
  - No claim mununu found a bug in any real device.
  - The `VIOLATED` verdict on `sending_reachable` is honestly described as the expected outcome under chaotic-stub default, not a finding.
  - The register map is described as illustrative, not derived from any commercial silicon.
  - The hand-authored firmware is described as a stand-in for libclang C extraction (Task C5, deferred).
- [ ] **Gate 4 — second reviewer.** A second reviewer (human or `review-orchestrator` agent) has read `substack.md` end-to-end and confirmed that the "what this example does not claim" caveats are not below the fold.

Only proceed to posting after all four gates pass. LinkedIn publishes only after Substack is live (it links to the Substack).

## Sequence for posting

1. Verify all four gates above are checked.
2. Post `substack.md` to the chosen Substack publication; capture the canonical URL.
3. Replace `[Substack link TBD]` in `linkedin.md` with the canonical URL.
4. Post `linkedin.md`; capture the canonical URL.
5. Update this README with both canonical URLs under a new "Published" section.
6. Commit + push so the version-controlled drafts point at the live posts.

## Position in the four-document arc

This is the **capstone post** of a four-post Substack series:

- **Post 1 (M1):** Black-box modules in compositional extraction → [`secure_boot_rom/publications/`](../../secure_boot_rom/publications/).
- **Post 2 (M2):** Two RTL frontends, one IR → [`dual_frontend_soc/publications/`](../../dual_frontend_soc/publications/).
- **Post 3 (M3):** A contract corpus for hardware verification → [`tls_handshake/publications/`](../../tls_handshake/publications/).
- **Post 4 (this one, M4):** Formal verification across the HW/SW boundary.

The capstone post pitches the whole stack via the codesign use case — pure-RTL verification is necessary but not sufficient; the properties that bite cross the boundary, and you need the prior three documents' machinery to verify those properties soundly.
