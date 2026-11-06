# Consumer briefing — the API refuses or applies six request fields it used to ignore (2026-11)

> Source of truth: [`parse_choice`](../../crates/mununu-core/src/api/handlers.rs) + [`parse_counter_bounds`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — the HTTP routes listed below; on the CLI only `sv verify-auto --counter-bound` changes (clap already validates the CLI's other options).

**Audience:** rosf, monono, mununu-ui, and any HTTP client of the routes below.

## TL;DR

The class is mununu#862's: a request field the route accepted and never read, or read and silently replaced with a default. Every site found by the sweep (mununu#865) now either applies the field or answers **400 `BAD_REQUEST`** naming the field. A request that used defaults or documented values is unchanged.

| route | field | before | now |
|---|---|---|---|
| `POST /api/v1/context/summarize` | `format: "table"` | ignored (JSON returned) | 400 |
| `POST /api/v1/context/synth` | `options.diagnostics.counterstrategy` | ignored (always computed when unrealizable) | applied; **defaults to `true`**, so a request without it is unchanged; `false` skips the computation |
| `POST /api/v1/context/import` | `filename` under `format: "auto"` | ignored (content sniffing only) | a known extension picks the adapter first, as in the CLI's loader |
| `POST /api/v1/sv/verify-auto` (and `sv verify-auto --counter-bound`) | an entry of `counter_bounds` that does not parse | dropped | 400 / CLI error naming the entry |
| `POST /api/v1/btor2/verify-liveness`, `POST /api/v1/sv/verify-liveness` | `grant` beside `responses` (or, on btor2, beside `justice`) | ignored | 400 |
| `POST /api/v1/btor2/cegar`, `POST /api/v1/sv/cegar` with `engine: "symbolic"` | `controllable`, a non-default `predicate_source` / `must_edge_inference` / `may_edge_inference`, `emit_ctxdsl: true` | ignored by the single-shot path | 400 |
| the CEGAR routes, `sv/verify-auto`, `sv/mutate` | an unknown `must_edge_inference`, `may_edge_inference`, `predicate_source` or `engine` string | silently the default policy | 400 listing the accepted values |

**`must_edge_inference: "sampling-confluence"`** was documented on the CEGAR routes but has had no engine behind it, and was silently `off`. It is now a 400. mununu-ui offers it in three dropdowns; its companion PR removes it.

No JSON shape changes. No verdict changes for a request that was evaluated as asked.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **mununu-ui** | The CEGAR, SV-CEGAR and SV-verify-auto runners offer `sampling-confluence`, which is now a 400. The CEGAR runners send every field on every run; the defaults (`wp` / `off` / `off`) are accepted with `engine: "symbolic"`. | merge the companion PR (drops the option) |
| **rosf** | none found: its fixtures use `engine: "exact-symbolic"`, a known value | none |
| **monono** | none found: it passes `--counter-bound 'cnt_q<=7'`-shaped entries, which parse | none; a malformed entry would now fail the run instead of being dropped |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if a client sends one of these fields | the routes' behaviour is in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow sends them | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes if you run `sv verify-auto` with `--counter-bound` in the image | the CLI parser changed |
| `mununu/docker/Dockerfile.profile` | no | profiling image |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not serve the API |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | only if rosf pins a mununu version and sends one of these fields | — |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | only if monono pins a mununu version and passes a malformed `--counter-bound` | — |

## Provenance

- **The sweep.** It covered every request type on the routes in `server.rs`, after #864 fixed `context/eval`'s `hide` / `minimize` / `stubs`.
- **The reproducer.** `crates/mununu-core/tests/api_unread_fields_865.rs` (6 tests, through the handlers). On the parent each field is accepted and ignored; on the fix each is applied or refused by name.
- **Unit tests.** The counter-bound parser and the string-choice parser have their own, in `make ci`; their routes need slang for an end-to-end run.

## Not covered here

The CEGAR routes default `may_edge_inference` to the sampled relation, which the core documents as unsound for safety; the CLI defaults to the sound one. That is a soundness change with its own reproducer, mununu#867. This change only makes an unknown spelling a 400.
