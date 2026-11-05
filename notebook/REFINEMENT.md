# Refining the Experimental Setup

The reproducibility scaffold itself is a work in progress. As we run experiments we will discover that:

- The hardware fingerprint is missing a field a reviewer asked about.
- The manifest needs a new provenance dimension (RUSTFLAGS, bench-target list, dhat sha).
- A bench harness signature needs to change.
- A fixture format must evolve (e.g., new persistence version).
- Replay needs to support a new container variant.

This document is the playbook for **changing the scaffold without breaking already-archived experiments**.

## Three classes of change

### 1. Additive optional change — no version bump

You're adding a *new optional field* to `manifest.json`, a *new optional file* to the EXP directory, or a *new script* that consumes existing archives.

Procedure:

1. Edit the producer (`scripts/bench_record.sh` or a new script).
2. Update consumers (`scripts/check_repro.sh`, `scripts/plot_speedup.py`) to **tolerate absence**.
3. Append a "Optional fields" entry under the current version section in `experiments/SCHEMA.md`.
4. Done. No history changes.

Example: adding `"bench_targets": ["mu_calculus_only::deep_fixpoint"]` to manifest. Old archives lack the field; consumers that need it fall back to parsing the criterion archive.

### 2. Additive required change — schema version bump

You're promoting a previously optional field to required, or adding a new field that future archives must carry to be replayable.

Procedure:

1. Append a new section to `experiments/SCHEMA.md` describing the new version.
2. Bump `schema_version` in `scripts/bench_record.sh`.
3. Add a new entry to the `REQUIRED` map in `scripts/check_repro.sh` keyed by the new version number. **Do not change** entries for older versions.
4. Add an ADR in `notebook/decisions.md` explaining why the new field is required.
5. Old archives at lower versions continue to validate green; new archives carry the new field.

Example: promoting `seed` to required when the bench harness wires `RandomClts` in. EXP-0001 stays at v1 (no seed); EXP-0002 onward is v2 with seed required.

### 3. Breaking change — supersede, don't rewrite

You're changing a bench harness signature, fixture format, or fundamental measurement methodology in a way that older archives can't replay.

Procedure:

1. Open a *new* EXP-NNNN that re-runs the affected measurement under the new methodology.
2. In the new EXP's `notes.md`, write a `## Supersedes` section explaining what changed and why.
3. In the old EXP's `notes.md`, append a `## Superseded by EXP-NNNN-<slug>` entry with the date and rationale.
4. Optionally add `"superseded_by": "EXP-NNNN-<slug>"` to the old `manifest.json` (optional field; no bump).
5. Add an ADR to `notebook/decisions.md`.
6. Update any blog post or paper draft that cited the old archive to point at the new one (or to acknowledge both: "v1 archive shows X under the old methodology; v2 shows Y under the corrected methodology, here's the difference").

The old archive stays in the tree forever as historical evidence. We never lose data.

## Anti-patterns

- **Rewriting `experiments/EXP-NNNN/` content in-place.** Even fixing a typo in a closed EXP's `log.md` should be done with a follow-up "Errata" section, not by editing the original prose. Treat archives as append-only the moment they ship.
- **Silently widening "required" without a bump.** A reviewer who reproduces in 2027 against a v1 archive shouldn't see "missing field X" because we added X to a new version's REQUIRED map and forgot to gate it.
- **Making `check_repro.sh` strict on optional fields.** Optional means optional. If a consumer needs the field for a particular plot or analysis, fail there with a clear message, not in the global gate.
- **Mixing scaffold changes with experiment results in one commit.** Scaffold edits and experiment archives belong in separate commits so `git log -- experiments/` and `git log -- scripts/` each tell coherent stories.

## Refining the bench harness

The bench harness itself (`crates/mununu-core/benches/_common.rs` and the per-subsystem benches) will evolve more often than the manifest. Some change classes:

- **New fixture template** in `test_support.rs`: additive, no version bump. Old benches that don't use it stay green.
- **Changing a fixture's deterministic seed**: BREAKING. The same `seed=0xC0FFEE` must always produce the same `Clts`. If the random generator changes (e.g., switching from `ChaCha20Rng` to `ChaCha12Rng`), bump a *fixture-format version* and bump the `schema_version` of any archives recorded against it. Old archives are superseded.
- **New bench function** (e.g., `composition_only::async_grid_50x50`): additive, runs under the same bench file. No version bump needed.
- **Renaming a bench function**: breaking for `bench-compare` against an old baseline. Treat as a methodology change; supersede old archives.
- **Changing Criterion config** (sample count, warmup, statistical methodology): bump the schema version and supersede affected archives. The change must be documented in an ADR.

## Refining the publication artifacts

- Blog post drafts (`mununu-private/publications/blog/`, moved out of this repo on 2026-11-05) are mutable until they ship publicly. Once a post is published (URL exists outside the repo), treat it like an archive — fix bugs via "Errata" appendices.
- The paper outline (`mununu-private/publications/paper/outline.md`) is mutable until camera-ready submission. After submission, edits become errata.
- Figures (`experiments/EXP-NNNN-<slug>/figures/`, beside the archive they come from) are regenerated from the archive and the plotting script. If a figure changes (e.g., switched from log to linear y-axis), keep the new figure alongside the old one with a `-v2.svg` suffix and reference both in the post.
