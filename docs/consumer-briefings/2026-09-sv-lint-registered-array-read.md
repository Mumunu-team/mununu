# Consumer briefing — 2026-09 `sv lint` gains a second rule, and findings gain `rule` / `detail`

> **Audience:** monono (direct CLI consumer; runs `sv lint` in its formal gate — and the design this rule comes from), ROSF (API consumer via `POST /api/v1/sv/lint`), mununu-ui, any orchestrator that parses `sv lint` findings.
>
> **Related:** [mununu#496](https://github.com/Mumunu-team/mununu/issues/496).
>
> **TL;DR:** `sv lint` now runs **two** structural checks instead of one, and every finding carries a **`rule`** tag saying which fired (plus an optional **`detail`**). The new rule flags a **registered array read whose address register can change in the same cycle its data is consumed** — the fault that shipped a sprite bank shifted by one halfword, twice, in the same block. **Additive JSON only**; existing fields are unchanged. A consumer that ignores unknown fields needs no change, but **a clean design may now report findings it did not before**, so the CI gate can newly fail — which is the point.

## What changed

**New rule — `registered-array-read-moving-address`.**

```systemverilog
always_ff @(posedge clk) begin
    if (advance) a_q <= a_q + 1;   // the address register moves
    q <= mem[a_q];                 // registered read against it
end
```

`q` holds the word at the address `a_q` held *last* cycle, but a consumer reading
`q` alongside the live `a_q` sees a pair that never coexisted, and nothing in the
design records the correspondence. Purely structural — no bit-blast, no
properties, no environment; it is a query over the netlist `sv lint` already
builds, at the same ~lift cost.

**Satisfied by writing the supported form**, like the other lift-form checks —
register the address alongside the data:

```systemverilog
    q   <= mem[a_q];
    a_d <= a_q;        // <-- the tracking signal
```

Two shapes are deliberately **not** flagged: an address that is not a register
(nothing moving to mis-pair with), and a register held constant (`a_q <= a_q`, or
no `next` at all — it can never disagree with `q`).

**Finding shape — two additive fields.**

```jsonc
// POST /api/v1/sv/lint  →
{ "signals_flagged": 1, "registers_flagged": 1,
  "findings": [ { "signal": "q",
                  "kind": "register",
                  "rule": "registered-array-read-moving-address",   // NEW
                  "detail": "`q` is a registered array read addressed by `a_q`, which can change in the same cycle ..." } ] }  // NEW, optional
```

- **`rule`** — `"undriven-partial-write"` | `"registered-array-read-moving-address"`. Always present on the API and CLI JSON. On the Rust type it carries `#[serde(default)]` to `undriven-partial-write`, so deserialising an older payload still works.
- **`detail`** — human-readable specifics, naming the address register and the fix. **Omitted entirely** when a rule needs no elaboration (so absent on every `undriven-partial-write` finding).
- Findings are now sorted **by rule, then by name** (was: by name).

## What did NOT change

- `signal` and `kind` — identical meaning and values.
- `signals_flagged` / `registers_flagged` — same counting.
- Exit codes: a finding is still the shared CI gate's `violated`, so `--fail-on violated` (default) exits `2`, a lift failure exits `1`, clean exits `0`.
- The `undriven-partial-write` rule's behaviour — byte-for-byte the same findings on the same designs. Its regression fixture is unchanged and the two rules are cross-tested for independence.
- No verdict, no route, no CLI flag, no sidecar schema, no engine behaviour. `sv lint` remains read-only.

## For monono

**This rule exists because of your postmortem** (`docs/postmortem-v04c-sprite-path.md`), and the fault it names occurred twice in the same block — the second time with the prose rule already in `CLAUDE.md`. That is the whole argument for making it structural.

**What to update:**

1. **Run it and expect hits.** `mununu sv lint <rtl> --frontend slang`. Any hit is a real instance of the pattern; the preloader and `sprite_bank` paths are the obvious first place to look.
2. **Fix by naming the tracking signal** — `a_d <= a_q;` alongside the read, and consume `a_d` rather than the live `a_q`. That is the form the check is satisfied by, and it is the same rule your `CLAUDE.md` already states in prose.
3. **If you filter findings by `kind`, add a `rule` filter too.** A gate that treated every finding as a partial-write problem will now mis-describe array-read findings in its output.
4. **Expect the gate to fail where it passed.** That is the rule working. Do not suppress it; each hit is either a real mis-pairing or a place where the correspondence is recorded in a way the check cannot see — and the second case is worth reporting back, because it is a false positive we should narrow.

**Report-parsing impact:** additive. If your parser rejects unknown JSON fields, relax it; otherwise no change.

**Docker rebuild:** yes if you want the new rule — it is a binary change. See the table.

## For ROSF

`POST /api/v1/sv/lint` responses gain `rule` (always) and `detail` (optional). Additive; a consumer ignoring unknown fields is unaffected. If ROSF surfaces lint findings to a user, showing `rule` and `detail` makes them actionable — `detail` already contains the address register and the suggested fix.

## For mununu-ui

`SvLintFinding` in `src/api/endpoints.ts` gains `rule: SvLintRule` (required) and `detail?: string` (optional), with a new exported `SvLintRule` union. Any code constructing an `SvLintFinding` literal must add `rule`. Shipped in this PR.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | carries the new rule; `sv lint` may report new findings | **Yes** — to get the rule |
| mununu `Dockerfile.dev` | binary bump | **Yes** — if the dev workflow runs `sv lint` |
| mununu `Dockerfile.sva` | binary bump; the e2e regression for this rule runs here | **Yes** — to run the `#[ignore]`d e2e |
| mununu `Dockerfile.extract`, `.extract-*` | no impact (no `sv lint` path) | No |
| rosf `Dockerfile` / `Dockerfile.dev` / `.hw` | consumes the API; response gains two fields | **No** — additive; rebuild when adopting the rule |
| monono Docker (if any) | primary beneficiary | **Yes** — this is the rule you asked for |
| mununu-ui deployment | type-only change | **No** — rebuild on next UI deploy |

## Verification steps

```bash
# Unit — the structural query, against captured BTOR2 (no slang needed):
cargo test -p mununu-core --lib sv_verify

# End-to-end through the REAL slang lift, in the pinned image:
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work \
  -e CARGO_TARGET_DIR=/cargo-target mununu-sva \
  bash -c 'export PATH=$HOME/.cargo/bin:/opt/oss-cad-suite/bin:$PATH; \
           cargo test -p mununu-core --lib e2e_sv_lint -- --ignored'
```

Both pass. The e2e test lifts the faulty design **and its satisfying twin** with
yosys-slang and asserts the first is flagged and the second is not — so the rule
is pinned against the real lift, not only against a captured fixture. Per the
CLAUDE.md rule, every slang-touching change here was validated in `mununu-sva`,
never on the bare host.

## Provenance

- Issue: [mununu#496](https://github.com/Mumunu-team/mununu/issues/496), filed from monono's `docs/postmortem-v04c-sprite-path.md`.
- Core: `lint_registered_array_read_moving_address` in `crates/mununu-core/src/adapter/sv_verify.rs`.
- Docs: [`docs/verifying-rtl.md`](../verifying-rtl.md) §"Registered array reads against a moving address", [`docs/cli-cookbook.md`](../cli-cookbook.md).
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here (follow-ups)

- **Tracking signals the check cannot see.** The rule recognises exactly one satisfying form: some register whose `next` *is* the address register. A design that records the correspondence differently — a wider tag, a shift register, an address reconstructed downstream — will be flagged as a false positive. The issue anticipates this ("false positives are expected wherever the design *does* record the correspondence"); narrowing wants real examples, so report them rather than suppressing.
- **Multi-cycle reads.** Only a one-cycle registered read is modelled. A two-stage read pipeline where the address must be delayed *twice* is not distinguished from a correctly-tracked one-stage read.
- **Write-side mis-pairing.** The dual fault — `mem[a_q] <= d` where `d` corresponds to a different `a_q` — is not checked.
- **Address cones, not just direct registers.** An address that is a combinational *function* of a moving register (`mem[a_q + 1]`) is currently out of scope; only a bare register address is flagged.
