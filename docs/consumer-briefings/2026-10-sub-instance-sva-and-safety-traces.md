# Consumer briefing — 2026-10 sub-instance SVA checked per instance, a disable at the attempt's start, safety counterexamples, value-preserving register names

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) — ROSF's SVA lane, monono's `make formal`, `--expect-*` gates — and anyone reading register names from `btor2 check-fsm`, `sv check-fsm`, `sv mutate --list`, sidecars or `btor2 verify --witness`.
>
> **Related:** [mununu#708](https://github.com/Mumunu-team/mununu/issues/708), [mununu#709](https://github.com/Mumunu-team/mununu/issues/709), [mununu#710](https://github.com/Mumunu-team/mununu/issues/710), [mununu#711](https://github.com/Mumunu-team/mununu/issues/711). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change (three defect fixes) plus a payload addition — re-baseline required.** (1) An assertion written inside a sub-instance is now checked against THAT instance's signals, once per instance; before, it read the top-level signal of the same name (a false HOLDS was possible) or skipped. (2) A register is named only by a value-identical alias; a bit-swapped or inverted alias used to be read as the raw register (a false HOLDS was possible). (3) A kept `disable iff` now also disables the attempt where it starts. (4) Every VIOLATED safety property carries its reset → violation trace.

## What changed

- **mununu#709 — instances** (`adapter/slang/translate.rs`): assertions are collected once per instance (a body several instances share is walked per instance; a generate branch the elaboration did not take contributes nothing); every reference declared in the assertion's body is named with the flattened instance path (`u_rx.u_dec.gen_no_async.diff_pq`), and an input port reads the expression connected to it in the parent (a logic-free checker module — `bind` or an instantiated `*_sva` module — keeps working: its ports have no net once flattened); a hierarchical reference `u_rx.state_q` from the top resolves when its module has one instance (refused with the reason when several share it, or the path crosses a `for`-generate iteration); enum members and parameters fold by their declaration, not their name (`Width` is 6 in one module and 2 in another; the first declaration used to win). A reset a sub-instance's `disable iff` names is pinned through the input it aliases.
- **mununu#711 — register names** (`btor2/parser.rs::collect_symbols`): a name is forwarded to a register only along a value-preserving path (a `uext`/`sext` by 0, a passthrough output, the async-reset mux). Any other named node keeps its own name, and every engine binds it to that node's exact value.
- **mununu#708 — disable at the start** (`slang/assume_guarantee.rs::guard_with_disable`): a body that evaluates nothing at its first cycle (`##1 req |-> …`) is also guarded as a whole, `(D || …)`.
- **mununu#710 — counterexamples** (`slang/verify_auto.rs::attach_safety_witnesses`, `btor2/bad_monitor.rs::emit_bounded_safety_witness_monitor`): native BMC re-derives the shallowest reset → violation path for every VIOLATED safety property without one. The BMC witness now lists the inputs of every frame including the last (`inputs.len() == prefix.len()`), and names registers through their value-identical aliases.

## What to expect

- **More decided properties on hierarchical designs.** OpenTitan's `prim_esc_rxtx` FPV harness (upstream 558921c, harness assumptions applied): before — 9 HOLDS, 1 VIOLATED (spurious), 1 unknown, 40 skipped, 5 unsupported; after — 54 HOLDS, 2 VIOLATED (`FsmLiveness*`, which rely on the unapplied liveness assumption `FiniteEsc_M`), 7 skipped (an FPV-only signal), 0 unknown, 2 unsupported. The spurious VIOLATEDs are cross-checked with SymbiYosys (abc pdr PASS).
- **Names of sub-instance properties change.** `name` carries the instance path: `u_rx.u_dec.prim_diff_decode_sva_12`. Top-level names are unchanged except for their positional index, which shifts when a shared body adds per-instance entries before them, or when an un-elaborated generate branch's assertions disappear. The `label` is unchanged — pin by label; a module instantiated twice now yields its label twice, once per instance.
- **Verdict flips are corrections.** A previously decided sub-instance assertion can flip in either direction (it was read against the top's same-named signal); an assertion over a permuted/inverted alias of a register can flip; a `disable iff` property starting with `##1` can go VIOLATED → HOLDS.
- **`counterexample` appears on safety VIOLATED rows** (additive; same `CounterexampleView` shape, empty `cycle`). The CLI prints `counterexample (reset -> violation):` with per-cycle inputs.
- **Register display names** in `check-fsm` / `sv mutate --list` / sidecar resolution: a register no value-identical alias names now stays anonymous rather than taking a derived wire's name (e.g. its next-value function `cnt_d`).
- **`btor2 verify --witness`**: one more input frame (the one the `bad` condition read) and more registers named.

## What to update, per consumer

- **ROSF** / **monono**: re-baseline `--expect-count` and any positional `name` pins of sub-instance assertions (prefer `label`); re-check a pinned verdict on any assertion inside a sub-instance or over a port alias — if it moved, the old verdict was read against the wrong signal; a JSON parser sees `counterexample` on more rows (additive).
- **mununu-ui**: no SV verify-auto view consumes these fields — nothing required.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator, naming, witness, CLI rendering | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e runs here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- a_kept_disable_also_guards_the_attempt_start hierarchy_forms_translate_from_real x711_ safety_witness a_ports_alias_resolves
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture \
  e2e_a_sub_instance_assertion_reads_its_own_signals e2e_a_disable_at_the_attempt_start e2e_a_bit_swapped_port
```

## Provenance

PR: pending — branch `fix/disable-iff-empty-cycles`. `Repro: in-repo` — `a_kept_disable_also_guards_the_attempt_start`, `e2e_a_disable_at_the_attempt_start_disables_it`, `hierarchy_forms_translate_from_real_slang_json`, `e2e_a_sub_instance_assertion_reads_its_own_signals` (main reports the sub-instance's `q == 0` HOLDS — read against the top's `q`), `x711_an_inverted_alias_is_not_read_as_its_register` (main: HOLDS), `x711_a_name_is_forwarded_to_a_register_only_along_an_identity`, `witness_is_the_concrete_counting_path`; each fails on main 7a48fad (checked in a worktree).

## Not covered here

- Signals inside a `for`-generate iteration (`gen[1].x`): refused with the reason; bracketed names are not in the atom grammar yet.
- A hierarchical reference into a module instantiated more than once: refused (slang names the declaration, not the instance).
- Liveness counterexamples beyond the existing bare-`AF` stall lasso.
