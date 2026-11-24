# Consumer briefing: `sv cegar` on the command line takes the lift arguments of the other `sv` verbs (2026-11)

> Source of truth: [`sv_cegar`](../../crates/mununu-cli/src/main.rs#L4737) — surface: CLI — `mununu sv cegar`: the lift flags, the `lift_notes` of its report, the refusal of a multi-clock design. The API route already lifts this way (`lift` in the request), so there is no API change.

**Audience:** anyone who runs `mununu sv cegar` from a script, or reads its `--json` report.

## TL;DR

- **No verdict changes on a design that lifted before (mununu#1190).** The command still lifts the same BTOR2 from the same file, `--source` files and `--top`, and runs the same refinement loop. Its report can now carry `lift_notes` (next item but one).
- **New flags on `sv cegar`**, the ones `sv verify-auto` and the other lifting `sv` verbs take, with the same meaning: `--design-dir`, `--exclude`, `--search-path`, `--include-dir`, `--param`, `--lift-override`, `--cutpoint`, `--merge-clock-domains`. The positional file is optional under `--design-dir`. Until now the command took a file, `--source` and `--top` only, so a design that needed a search path, a parameter or a cut could be given to the API route and not to the command.
- **New key in the `--json` report: `lift_notes`.** It carries the notes the other `sv` verbs print for their lift (`clock-domains-merged` and the other clock notes, `blackbox-cut`, `control-slice`, `parameter-override`, `lift-retry`), in the same shape (`kind`, `level`, `summary`, `detail`, `items`), and is absent when the lift has none. In text mode the same notes are printed as lines after the report.
- **A design with more than one clock domain is refused with a fix on this verb.** The refusal used to say that this operation has no opt-in. It now offers `--merge-clock-domains`, as the other verbs do; with the flag the run goes on and the report carries the `clock-domains-merged` note, which says what a verdict on the merged model is worth.
- **One refusal is new.** `--design-dir` together with a `--sidecar` that declares `simulate_reset` is refused: the reset simulation needs the top module's file, and a directory does not name one. The message names what to pass instead (the file as the positional argument, with `--search-path` for the other sources and `--include-dir` for include fragments). Without `--design-dir` a `simulate_reset` sidecar works as before.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | New flags; `lift_notes` may appear in the JSON report; the multi-clock refusal reads differently. | None unless a lane matched the old refusal text. A lane that worked around the missing flags (a hand-flattened file, a pre-processed include tree) can drop the workaround. |
| **monono** | Same. | Same. |
| **mununu-ui** | No UI affordance for the RTL verbs. | None. |

## Docker rebuild

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | They ship the `mununu` binary. | Yes, to get the flags. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | They build from source per use, or do not verify. | No. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | They consume the binary. | When a lane wants the new flags on `sv cegar`: bump the pin. Otherwise no. |

## Check it on your side

Run `mununu sv cegar --help`: the flags above are listed. On a design of yours that has a parameter, `mununu --quiet sv cegar <file> --top <top> --param <NAME>=<VALUE> --formula '<formula>' --predicate '<reg> == <value>' --json` prints a `lift_notes` entry of kind `parameter-override` whose `items` name the override.

## Provenance

- **Change:** pending merge — see branch `feat/sv-cegar-lift-args-1190`. Issue: [mununu#1190](https://github.com/Mumunu-team/mununu/issues/1190). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- **Found by:** mununu#1190, from the review of the clock refusals: `sv cegar` was the one lifting `sv` verb whose API route took the shared `lift` object while the command built its lift by hand.
- **engine:** predicate-cube KMTS (`explicit` by default on this verb, `symbolic` by flag), 3-valued modal-μ with CEGAR, cvc5 or the owned SMT path for the abstract transitions; unchanged. The change is in what reaches the lift (yosys-slang to BTOR2) before any engine runs.
- **Tests:** `every_sv_verb_that_takes_the_merge_flag_takes_every_lift_argument`, `sv_cegar_parses_the_lift_arguments_with_a_file_or_a_design_dir`, `a_sidecar_asks_for_a_reset_simulation_exactly_when_it_carries_the_block`, `a_reset_simulation_sidecar_under_design_dir_is_refused_with_flags_of_the_verb`, and over the binary the four `cegar` rows of `every_sv_verb_under_merge_clock_domains_prints_the_merged_domains` and `sv_cegar_lifts_the_design_its_lift_arguments_describe`.

## Not covered here

- **`sv discover` and `sv emit-btor2-per-module`** still build their lift by hand and take a file, `--source` and `--top` only. Their clock refusal says they have no opt-in.
- **A sidecar's `simulate_reset` given with no source file at all** is skipped without a word on the other paths (mununu#1263); this change only refuses the one combination it would have added to that.
- **A sidecar that does not parse** is dropped whole without a word (mununu#1267), on this verb as on the others.
