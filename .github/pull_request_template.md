## What and why

<!-- What changed, why, and what was measured. Lead with the outcome. -->

<!-- Authored-on: this machine's nickname (MUNUNU_MACHINE_NICK), never a hostname.
     Class: for a fix only — a slug of docs/defect-classes.md, or `none — <why this is a single site>`; delete the line otherwise. -->
Authored-on:
Class:

## Checklist

- [ ] **Fixes a defect?** The `Class:` line above is filled in, the class's cases are enumerated from the code that produces them, and its entry in `docs/defect-classes.md` is updated (`docs/policies/fix-the-class.md`).
- [ ] **Sensitive** (`python3 scripts/pr_review_gate.py --pr <N>` says so)? It waits for `Cross-review: approve` from a session on the other machine, at the current head (`docs/policies/cross-host-review.md`).
- [ ] **Fixes a defect?** Each fixing commit carries exactly one `Repro:` trailer (`in-repo <test>` · `consumer-only <who> — <why>` · `none — containment only`), and the squash message keeps one per fix. `make verify-repro TEST=<name>` says CONFIRMED, or the PR says how it was checked by hand.
- [ ] **Adds or changes a `pub` field of a struct with several construction sites** (`VerifyAutoOptions`, `CegarOptions`, `TransitionSpec`, …)? The workspace check ran: `cargo check --workspace --all-features --tests`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo test --workspace --all-features --doc`.
- [ ] **Changes a surface a downstream repo consumes** (a verdict, a JSON shape, a CLI flag, an HTTP route, a subprocess tool)? One consumer briefing under `docs/consumer-briefings/`.
- [ ] **User-visible capability?** CLI and API in this PR (`/parity-check`); docs anchored (`/docs-traceability`).
