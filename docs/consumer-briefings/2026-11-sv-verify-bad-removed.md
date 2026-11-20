# Consumer briefing: `sv verify-bad` and `POST /api/v1/sv/verify-bad` are removed (2026-11)

> Source of truth: [`SvCommand`](../../crates/mununu-cli/src/main.rs) and the `sv` routes in [`server.rs`](../../crates/mununu-core/src/api/server.rs) — surface: CLI+API — the verb and the route no longer exist; `sv verify-auto` / `POST /api/v1/sv/verify-auto` is the SystemVerilog safety surface, `btor2 verify` / `POST /api/v1/btor2/verify` the BTOR2 one

**Audience:** anyone who calls `mununu sv verify-bad` or `POST /api/v1/sv/verify-bad`, or who matches on the README's surface table.

## TL;DR

- **Removed (mununu#1074).** Since the slang-only lift (2026-11-04) the verb lifted the design with `read_slang --ignore-assertions`, so every SystemVerilog input lifted to a model with no `bad` line and the verb refused it ("the lifted design has no `bad` line, so there is nothing to decide …") — a surface that could not succeed. Confirmed by one run in the image before the removal (a module with an immediate `assert` and a concurrent `assert property`: the refusal, nothing else). No consumer was found calling it (rosf, monono, mununu-ui).
- **What to call instead:** for a module's assertions, `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto` (the SVA property set, the portfolio, the notes); for a hand-written `bad` line, `mununu btor2 verify` / `POST /api/v1/btor2/verify` on the BTOR2.
- **Shape change:** the route is gone (`404`); the CLI verb is gone (`mununu sv verify-bad` is an unknown subcommand, exit 64); `docs/api-schemas/sv-verify-request.schema.json` is removed. Nothing else changes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | None found (no call to the verb or the route in the repository). | None. |
| **monono** | None found. | None. |
| **mununu-ui** | None (no client for the route). | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to drop the surface. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** mununu#1074 (the question "does `sv verify-bad` keep a role now that the lift drops every assertion?"), with the reading and the three options on the issue; option 1 taken on 2026-11-20 after the one-run falsifier.
- **Tests:** the CLI routing test no longer accepts the verb; the API schema-drift test no longer has a `sv-verify-request` entry. No reproducer: a removal, not a fix (`Repro: none`).

## Not covered here

- The gap behind the question — no verb checks immediate assertions; `sv verify-auto` only reports them as dropped (mununu#988) — is not changed by this removal.
