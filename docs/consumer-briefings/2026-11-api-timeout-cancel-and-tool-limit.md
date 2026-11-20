# Consumer briefing: a timed-out API request cancels its work; the tool routes have a concurrency limit (2026-11)

> Source of truth: [`ApiLimits`](../../crates/mununu-core/src/api/server.rs) and `blocking` in [`api/handlers.rs`](../../crates/mununu-core/src/api/handlers.rs) — surface: CLI+API — `mununu server --request-timeout` / `--max-tool-requests`, every `/api/v1/*` route

**Audience:** anyone who runs `mununu server` (the on-prem bundle, the rosf and monono containers) or sends more than a few verify requests at once.

## TL;DR

- **A request past the timeout now stops its work (mununu#1147).** Before, a 408 left the lift's yosys and the engine threads running to their own end. Now the handler's run budget is cancelled when the response is dropped; the engines, the planner and the subprocess helper (yosys, btormc, Pono, rIC3) stop within a poll interval. The 408 body is unchanged.
- **Two new `mununu server` flags:** `--request-timeout <seconds>` (default 30, the value that was hard-coded) and `--max-tool-requests <n>` (default 4).
- **A new error code:** 503 `SERVER_BUSY` on `/api/v1/sv/*`, `/api/v1/btor2/*`, `/api/v1/verify`, `/api/v1/codesign/verify` and `/api/v1/memory/check` when `n` such requests are already running: `{success: false, error: {code: "SERVER_BUSY", message: "the server is running its limit of 4 tool-running request(s); retry when one finishes, or start `mununu server --max-tool-requests <n>` with a larger limit"}}`. The refusal is immediate (no queue). The CTXDSL routes are not limited.
- **No verdict or response-shape change** on a served request.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A client that fires many `sv verify-auto` calls at once sees 503 past four; a timed-out call no longer keeps the host busy. | Retry on 503 (with backoff), or start the server with `--max-tool-requests` sized to the host. |
| **monono** | Same. | Same. |
| **mununu-ui** | The UI sends one verify at a time; a 503 would show as the generic error with the message above. | None required; optionally surface `SERVER_BUSY` as "busy, retry". |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the cancel and the flags. | They ship the `mununu` binary and start `mununu server`. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin; pass the flags if the defaults do not fit the host. | They consume the binary. |

## Provenance

- **Found by:** the R6 review (mununu#1147).
- **Tests:** `x1147_a_dropped_request_cancels_its_blocking_work`, `x1147_a_cancelled_run_budget_kills_the_child_it_waits_on`, `x1147_past_the_tool_request_limit_a_request_is_refused_by_name`.

## Not covered here

- `mununu mcp --request-timeout` is unchanged (its budget already names the over-budget call; its engine thread still finishes on its own — a follow-up if a consumer needs it).
- The limit counts requests, not threads: one portfolio run still spawns its members.
