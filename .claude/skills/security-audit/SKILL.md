---
name: security-audit
description: >
  Security audit of mununu's Rust code: unsafe, subprocess tool invocation,
  path handling (imports, design dirs, breadcrumbs, the UI static directory),
  resource exhaustion and budgets, the HTTP / MCP API, the licence and usage
  ledger surfaces, dependencies and advisories, deserialization of untrusted
  inputs, and the private-files exposure rules. Use when asked to audit,
  check security, or review for vulnerabilities; invoked by review-orchestrator.
---

Perform a security audit of $ARGUMENTS (with no arguments, the branch against `main` plus the working tree; name the files you did not open). Rules: `CLAUDE.md` §Code Reuse, Dead Code, Performance, Security (OWASP line), §Private Files Policy (`docs/policies/private-files.md`), §Failure Paths (`docs/policies/failure-paths.md`).

## Audit checklist

1. **Unsafe.** Expected: the CLI's `std::env::set_var` (edition 2024) in `crates/mununu-cli/src/main.rs`, and six process-supervision FFI blocks in `crates/mununu-core/src/adapter/mod.rs`, each with its `// SAFETY:` comment: three Win32 Job Object calls in `mod member_job` (create and assign, terminate, close) and three POSIX calls in `mod member_group` (`kill(2)` on a process group, the `signal` / `raise` re-raise, the `sigaction` query). Integration tests may `set_var` in a single-test binary. FFI reaches z3, OxiDD and tree-sitter through safe crates. Any other `unsafe` is a finding unless it is an FFI boundary with a `// SAFETY:` comment that argues the invariant.

2. **Subprocess tools** (`std::process::Command`): yosys + slang, sv2v (dev oracle only), btormc, pono, rIC3, Verilator, cvc5, docker. Arguments are separate `.arg()` calls built from parsed values, never a shell string; the tool path comes from the adapter's locate function (`MUNUNU_*_PATH` then `PATH`), never from a request body; the discovery probe runs under a 10 s budget (#876); a cancelled or timed-out member is killed with everything it forked (process group / Job Object, #875 / #922); tool output lands in a directory the run owns. A design's text never reaches a command line.

3. **Path handling.** CTXDSL imports and `@import`, `--design-dir`, sidecar paths, `--resume-from` (`adapter/resume.rs`: a breadcrumb is parsed NDJSON, build-checked before use), `MUNUNU_LIFT_CACHE_DIR` (content-addressed; a key collision is a soundness question, hand it over), and the newest surface **`mununu server --ui-dir`** (static files for the bundled UI): nothing outside the directory is served — `..`, absolute paths, symlinks out of the tree — check `api/server.rs` and test it with a traversal request.

4. **Denial of service and budgets.** Budgets are deterministic where they decide a verdict (`MUNUNU_BDD_TIME_BUDGET_MS` is off by default for that reason; the k-induction member uses a z3 `rlimit`, #917); node budgets and arena exhaustion abstain (`ResourceBudgetExceeded`, never a panic — `panic = "unwind"` in the workspace profile is load-bearing); API handlers re-parse per request and keep post-realization work light, never synthesis in a summary endpoint; request body size limits and timeouts on the axum routes; `/api/v1/sv/*` runs external tools, so the server binds loopback by default and `docs/docker.md` says `-p 127.0.0.1:8080:8080`; the MCP transport (`api/mcp.rs`, rmcp ≥ 3.9 for RUSTSEC-2026-0462) inherits the same limits.

5. **Licence and ledger surfaces** (`crates/mununu-core/src/licence.rs`, `usage_ledger.rs`; `docs/licensing.md`). Verification is Ed25519 against the embedded public key; no private key material anywhere in the tree (`git grep -n 'PRIVATE KEY'`, 64-byte hex seeds); the evaluation / grace / expiry decision reads only the verified file's fields and names its clock assumption; `licence status` and `GET /api/v1/licence` expose nothing beyond the file's public fields; the ledger is opt-in, local, counts and identities only, with a **closed key set** — a new key, or a design-derived string in a record, is a finding; the licence check never reaches a verdict (`tests/licence_never_reaches_a_verdict_h1.rs` is the pin).

6. **Secrets, private names, machine-local data.** No tokens or passwords; `.gitignore` covers `.env`; no partner / customer / person names in tracked files, comments, commit messages or PR text — the denylist lives in `mununu-private/docs/security/private-names.txt` (grep when the sibling checkout is present, else say `not run`); no absolute paths, home directories, hostnames or account handles in tracked files (`git grep -nE '/Users/|/home/[a-z]|C:\\\\Users' -- . ':!target' ':!docs/archive'`); consumer briefings name a consumer by repository.

7. **Dependencies.** `Cargo.lock` committed; CI runs `cargo audit` (the `security-audit` job), `cargo deny check licenses` (`deny.toml`, the copyleft gate), `cargo machete`; an advisory fix bumps the floor in `Cargo.toml` so the lock cannot drift back (rmcp 3.8 → 3.9, #927). Flag a `git` or `path` dependency, an unpinned major.

8. **Deserialization of untrusted input.** `serde_json` on sidecars, XState / CrewAI / LangGraph JSON, breadcrumbs, licence files, API bodies: size bounds, `#[serde(deny_unknown_fields)]` or an explicit unread-field report on public request types (`tests/api_unread_fields_865.rs`), no unbounded collections from a request; the API wire shapes are pinned by the drift test under `docs/api-schemas/`.

9. **Integer arithmetic on design-controlled values.** Widths, counts, bit positions and nids from a design use `checked_*` / saturating ops or the arbitrary-width `Bv`; a `u128` shift on a 135-bit cell crashed in debug and computed a wrong bit in release (mununu#848) — any `as u32` / `as usize` narrowing on such a path is a finding.

10. **Logging.** `tracing` at info and above carries no design content, file contents or tokens; paths only when the user supplied them.

## Output format

Severity **Critical** (exploitable or data-exposing now) / **High** / **Medium** / **Informational**, each with `path:line`, the checklist item, and the fix in one line. End with the checks not run and why.
