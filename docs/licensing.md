# The licence file (`mununu.lic`)

> Source of truth: [`licence::status`](../crates/mununu-core/src/licence.rs) (`mununu licence status`, `GET /api/v1/licence`, the `licence` field of `/api/v1/health` and of every `sv verify-auto` report) — surface: CLI+API

A self-hosted mununu is licensed by a **file**, not by a server: a signed `mununu.lic` names the
licensee, the team, the tier and the term, and the binary checks its Ed25519 signature against
the issuer's public key compiled into it. Nothing phones home, nothing fingerprints the host, and
nothing needs a network — a site with no outbound connection is licensed the same way as a
laptop. The licence is a legal instrument plus an honest status line; it is not copy protection.

**A licence never touches a verdict.** No engine, adapter, planner or evaluator reads the licence;
a test (`crates/mununu-core/tests/policy/licence_never_reaches_a_verdict_h1.rs`) keeps it that way. The
state is *reported* — on the status surfaces below and in every report, next to the build
identity — and the verdicts of a design are byte-identical in every licence state. What the tool
does in each state is decided once at start-up, by the CLI and the server, and nowhere else:

| state | what happens |
|---|---|
| `valid` | runs |
| `evaluation` — no licence file, within **30 days** of the first run on this host | runs; one stderr line on every start (`--quiet` hides it) |
| `grace` — a file past `expires`, within its `grace_days` | runs; a loud stderr line on every start |
| `expired` — a file past its grace days, or the 30-day evaluation over | `mununu` exits 1 with the renewal text; `mununu server` refuses to start |
| `invalid` — a file this binary does not accept (bad signature, tampered, unknown key) | treated as no file: the evaluation term governs, and the line says why the file was refused |
| `missing` — no file, and the first run could not be recorded | runs, says so |

The evaluation term is counted from a stamp the first run writes, `~/.mununu/first-run`
(`%USERPROFILE%\.mununu\first-run` on Windows), holding the date. A host whose home directory is
not writable cannot be counted and is reported as `missing`: this is a legal instrument and an
honest status line, not copy protection. `mununu licence status` always runs, in every state, so
the reason for a refusal is always readable; the free tiers (academic, individual, open source)
receive a licence file like everyone else — write to the maintainers.

## Where the file goes

The first match wins:

1. `[licensing] file = "mununu.lic"` in the deployment's `mununu.toml` (relative to that file;
   see [`deployment`](../crates/mununu-core/src/deployment.rs)), named by `--deployment-config`,
   `mununu server --config`, or `MUNUNU_DEPLOYMENT_CONFIG`;
2. the environment variable `MUNUNU_LICENSE_FILE`;
3. `/etc/mununu/mununu.lic`.

No file at any of the three is the `missing` state, reported as such.

## Reading the status

```bash
mununu licence status            # human-readable (also: mununu license status)
mununu licence status --json     # the same document as GET /api/v1/licence
curl -s localhost:8080/api/v1/licence
curl -s localhost:8080/api/v1/health | jq .licence   # {state, tier, expires}
```

```text
licence: valid
  licensee:  Example Ltd (team example-hw)
  tier:      team
  issued:    2026-11-08
  expires:   2027-11-08 (365 days left)
  key id:    mununu-2026-11
  source:    /etc/mununu/mununu.lic
```

| `state` | meaning |
|---|---|
| `valid` | signed by a trusted issuer key and within its term (`expires` is the last valid day, inclusive) |
| `grace` | past `expires`, within the file's `grace_days` |
| `expired` | past `expires` and past its grace days — or no file and the 30-day evaluation over (then `claims` is absent and `evaluation` says when it ended) |
| `invalid` | a file was found but is not a licence this binary accepts — `reason` says what is wrong (not a licence file, wrong format version, unknown issuer key id, signature does not match the claims, a bad date, unreadable); the claims are still shown when they parsed, so a file signed by a retired key still says whose it is; `evaluation` carries the term that governs instead |
| `evaluation` | no file at any of the three locations, within the 30-day term from this host's first run; `evaluation: {started, ends, days_left}` |
| `missing` | no file, and the first run could not be recorded (the home directory is not writable) — `reason` says why |

`days_left` counts to `expires` (`0` on the last valid day, negative after it); for the
evaluation it counts to the term's end. A `note` appears when `issued` is after today: the file
or the host clock is wrong. The schema is
[`docs/api-schemas/licence-status.schema.json`](api-schemas/licence-status.schema.json).

## The usage ledger (opt-in, local)

> Source of truth: [`usage_ledger::record`](../crates/mununu-core/src/usage_ledger.rs) (`sv verify-auto` on the CLI and `POST /api/v1/sv/verify-auto` each write one record; `mununu licence status` / `GET /api/v1/licence` show the path as `ledger`) — surface: CLI+API

Nothing phones home, so the evidence a renewal or a volume conversation stands on is a file **you**
hold. Name it and every verification run appends one JSON line:

```toml
[licensing]
file   = "mununu.lic"
ledger = "/var/lib/mununu/usage.ndjson"   # or the environment variable MUNUNU_USAGE_LEDGER
```

```json
{"ts":"2026-11-08T19:58:02Z","verb":"sv verify-auto","surface":"cli","version":"0.5.11","build":"74bca9f6…","licence_state":"valid","licence_tier":"team","licence_team":"acme-eng","result":"report","properties":44,"holds":40,"violated":2,"unknown":2,"skipped":0,"unsupported":1,"wall_ms":183420}
```

**What a record carries:** the time, the verb and surface, the binary's version and build, the
licence state / tier / team (the invoice key; tier and team only from a file whose signature
verified, never from an `invalid` one, mununu#938), the run's result (`report`, or `error` when the
run failed before a report — the counts are then zero), the property counts by verdict, the
unsupported count, and the run's wall. **What it never carries:** a file name, a path, a module
or property name, a formula, a counterexample or a note. The record type is a closed set of
fields; a test pins the exact key list (`RECORD_KEYS`) and another builds a record from a report
full of design names and checks that none of them reaches the line.

The ledger is **off** unless named. A write that fails (a missing directory, a read-only volume) is
reported — one stderr `warning:` line on the CLI, a `warn` in the server log — and the run is
unaffected; a ledger must not be able to fail a verification. `mununu licence status` prints
`ledger: <path>` or `ledger: off` with the two ways to turn it on.

Reading it needs no tool:

```bash
grep -c '"verb":"sv verify-auto"' usage.ndjson                       # runs this month
jq -s 'map(.properties) | add' usage.ndjson                            # properties verified
jq -r 'select(.result=="error") | .ts' usage.ndjson                    # the runs that failed before a report
jq -s 'group_by(.licence_team) | map({team: .[0].licence_team, runs: length})' usage.ndjson
```

The on-prem bundle mounts a directory for it (`deploy/compose.yaml`, commented out by default).
Mail the file, feed your own dashboard, or delete it; it is yours.

## The format

A JSON envelope over a signed payload:

```json
{
  "mununu_licence": 1,
  "payload": "<base64 of the claims JSON>",
  "signature": "<base64 of the Ed25519 signature over the decoded payload bytes>"
}
```

The claims: `licensee`, `team_id`, `tier` (`evaluation` or `team`), `issued` and `expires`
(`YYYY-MM-DD`, UTC), `grace_days`, and `key_id`, the issuer key that signed the file. The
signature is a plain Ed25519 signature (RFC 8032) over the payload's raw bytes, so any standard
signer can produce one; a claim this binary does not know is kept under the signature and
ignored, so an older binary still reads a newer file. Rotation is a new key id compiled into a
later release; a file signed by a key the binary does not carry is `invalid` with the id in its
reason.

Issuance is private: the signing key never ships. The issuer key this release trusts is
`mununu-2026-11`, Ed25519 public key
`cd37de5830daba16acb4a651534584e8ca238936f8b6848b3e241d03c9bb4fdf` (the value compiled into
`ISSUER_KEYS`). To obtain a licence, see the pricing page or write to the maintainers.
