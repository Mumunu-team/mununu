# Consumer briefing — 2026-10 a field read through an address mux decides on the exact engine: the antecedent's constant is propagated through the consequent

> **Audience:** monono first — this is the second half of your [#602](https://github.com/Mumunu-team/mununu/issues/602) (`rtl/vpu/vpu_status`), filed as [#629](https://github.com/Mumunu-team/mununu/issues/629) after the bit-level cone landed. Also ROSF and any consumer whose `sv verify-auto` reports a same-cycle `|->` property over a wide packed record as `skipped` (over the cap, or "atom references primary input").
>
> **Related:** closes [#629](https://github.com/Mumunu-team/mununu/issues/629). Builds on the bit-level cone of [#630](https://github.com/Mumunu-team/mununu/pull/630) (briefing [`2026-10-bit-level-cone-of-influence.md`](2026-10-bit-level-cone-of-influence.md)) and on the antecedent-shadow synthesis for `|=>` ([`docs/design/antecedent-shadow-synthesis.md`](../design/antecedent-shadow-synthesis.md)). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change (more properties decide; none moves) + one new opt-out flag.** For a same-cycle implication `sig == K |-> C`, the exact engine now propagates `K` through `C`'s cone before computing the cone — `AG(sig == K -> C(sig)) ≡ AG(sig == K -> C[sig := K])` — so a read mux folds to its selected arm and the bit cone is the one field. Your `vpu_status` shape (`(addr == 20'h4) |-> (rdata == status_word)`) decides on the exact engine: the correct design HOLDS, the "merge omitted" twin VIOLATED — a 2-valued refutation of a defect the abstracting engines left ⊥. An input antecedent is dropped with the rewrite when nothing else reads the input. `--no-antecedent-propagate` / `no_antecedent_propagate: true` / `MUNUNU_NO_ANTECEDENT_PROPAGATE=1` disable it. The "flags swapped" twin is rewritten the same way but its refutation hits the variable-order wall, not the cone — recorded below, not fixed.

## What changed

- **`adapter/btor2/antecedent_propagation.rs`** — for every implication `Or(Not(sig == K), ψ)` of positive polarity in the mu-formula, with `sig` a state cell or a primary input and `K` a constant the cell can hold: for each register `r` read by a consequent atom whose combinational cone reaches `sig`, a copy of that cone is minted with `sig` replaced by `K` and constant-folded (`eq` of two constants, `ite` on a constant condition — the mux chain collapses to the selected arm), named `r__<sig>_eq_<K>`, and the atom is renamed to read it (`rdata__addr_eq_4 == status_word`).
- **Input antecedents.** The exact engine leaves inputs free and quantified out by the modalities, so a state atom over an input is refused (it would decouple the atom's copy of the input from the transition's — the `|=>` shadow synthesis exists for that). For a same-cycle `|->` the universal reading the SVA means — ∀ input: `sig == K → C(sig)` — *is* `C[sig := K]` when every input value is admissible, so the implication is replaced by its rewritten consequent. That step is taken only when no `constraint` / `fair` / `justice` line reads the input, no atom of the formula still reads it after the rewrite, and the implication is of positive polarity; otherwise the antecedent stays and the engine's refusal stands, as today.
- **State antecedents** keep the implication (a state atom like any other); only the consequent is rewritten.
- Declined, formula left as lifted: a `Select` (array) atom in the consequent, a non-equality antecedent, a constant the register cannot hold.
- Opt-out on all three channels, mirroring the shadow knob: CLI `--no-antecedent-propagate` (`sv verify-auto`, `internal-engine-eval`), API `no_antecedent_propagate: true` on `POST /api/v1/sv/verify-auto` (request schema regenerated — additive), env `MUNUNU_NO_ANTECEDENT_PROPAGATE=1`.

**Soundness.** Per implication the rewrite is an equivalence on the model (the substitution lemma: where the antecedent holds `sig` *is* `K` in that cycle; where it does not, the implication is true regardless), so no verdict can move. The wall-class matrix is byte-identical with the pass on and off.

## What it does for `vpu_status`

The synthetic twin of your shape — 23 × 32-bit words crossed twice, a 23-arm address mux, `status_word` = word 4:

| design | as lifted (exact engine) | now |
|---|---|---|
| correct | refused — "atom references primary input `addr`" | **HOLDS** (cone 96 bits) |
| merge omitted — arm 4 serves the un-synchronised copy | refused | **VIOLATED**, with a witness |
| address registered instead of an input | abstained on the bit cap (2,213 bits) | **HOLDS** |
| flags swapped — arm 4 wired to word 5 | refused | rewritten, then **arena exhausted**: the refutation `ws[191:160] == ws[159:128]` is an equality between *misaligned* bits of one register, exponential under the engine's fixed interleaved variable order |

So two of your three contrast twins now have a 2-valued verdict where the explicit engine gave ⊥ (merge omitted) or had to be trusted at HOLDS (correct). The swapped-flags twin is **not a cone problem any more** — it is the ordering wall of [`docs/design/bdd-variable-ordering.md`](../design/bdd-variable-ordering.md), the first implication-shaped case of it, and it stays ⊥/abstained on the exact engine until that lever exists. Phrase a bit-order check on a single bit (`status_word[2] == …`) and it decides today.

## What to update, per consumer

### monono

- Re-run the `vpu_status` lanes without `--cutpoint`: expect `decided_by: "exact-symbolic"` on the SVA and on the merge-omitted twin. Your `verify.sh` `expect_named … HOLDS` pins that read the explicit engine's verdict see the same verdict from a 2-valued engine now; a pin on the *engine* needs updating.
- Nothing to change in RTL. A property that reads the field through the mux is the supported phrasing now; a slice-phrased one still works.
- Your JSON parser: one new optional request key, no response change.

### ROSF and others

- No response-shape change. More `holds` / `violated`, fewer `skipped`, on same-cycle `|->` properties whose antecedent is an equality on an address / select / mode signal.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | exact-engine property rewrite; new opt-out flag | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The pass on the consumer's shape, both twins, both antecedent kinds, the constraint guard:
cargo test -p mununu-core --lib --features api -- x629_
# 2. Identity on the fixed class set, with and without the pass:
cargo test -p mununu-core --test wall_class_matrix -- --ignored --nocapture
MUNUNU_NO_ANTECEDENT_PROPAGATE=1 cargo test -p mununu-core --test wall_class_matrix -- --ignored --nocapture
# 3. Your design: diff `sv verify-auto --json` with and without --no-antecedent-propagate — only
#    `skipped` → decided moves are expected.
```

## Not covered here

- The misaligned-bits equality (the swapped-flags twin) — a variable-order lever, tracked with the per-cone-order item of the engine-performance roadmap.
- `|=>` (next-cycle) implications keep the antecedent-shadow synthesis; composing the two (a next-cycle consequent read through a mux on a shadowed address) is not attempted.
- Only equality antecedents `sig == K` on a leaf; a combinational antecedent (`sig_q + 1 == K`) or a range (`addr < K`) is left as lifted.
