# External Tools

> Source of truth: [`docs/external-tools.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/external-tools.md) — the canonical, always-current install + integration reference. This wiki page is a summary; see the doc for per-OS install, discovery, and missing-tool behaviour.

Mununu's verification engine, model checker, and synthesiser are **pure Rust + the linked Z3 library**. Several pipelines additionally invoke external command-line tools. Every external tool is **optional** — mununu functions without it and emits a structured warning/error (or falls back) when a pipeline needs a missing tool, never silently failing.

## How each tool is integrated

| Tool | Used for | Integration | Install (macOS / Debian) |
|---|---|---|---|
| **Z3** | the SMT backend (everything) | **linked** (in-process, mandatory) | `brew install z3` / `apt install libz3-dev` (set `LIBRARY_PATH`) |
| **sv2v** | SystemVerilog preprocessing | discovered subprocess | `brew install zachjs-sv2v` / build from source |
| **Yosys** | SystemVerilog → BTOR2 | discovered subprocess | `brew install yosys` / `apt install yosys` |
| **SymbiYosys (sby)** | RTL verification oracle | discovered subprocess | bundled with Yosys (OSS-CAD-Suite) |
| **CVC5** | Craig-interpolation predicates (`--predicate-source craig`) | discovered subprocess | `brew install cvc5` / `apt install cvc5` |
| **Verilator** | reset-state simulation seeding (sidecar `simulate_reset`) | discovered subprocess | `brew install verilator` / `apt install verilator` |
| **circt-verilog** | alternative extraction frontend (`mununu-extract circt`) | pipe input | build CIRCT / release tarball |

Discovered subprocess tools follow one pattern: the `MUNUNU_<TOOL>_PATH` env var first, else the bare binary on `$PATH`.

## Two integration models — and why it matters for licensing

- **Linked (Z3 only):** compiled into the mununu binary via the `z3` Rust crate. Z3 is **MIT** (permissive) — safe.
- **Subprocess / pipe (everything else):** mununu runs the tool as a separate process. This is **"mere aggregation"** — the tool's license does **not** contaminate mununu's source license, even for copyleft tools (Verilator is LGPL-3 / Artistic-2). None are bundled into a mununu artifact; you install them yourself.

The only real license-contamination vector — the **linked Cargo crate graph** — is guarded in CI by `cargo deny check licenses` (a deny-by-default permissive allow-list; see [`deny.toml`](https://github.com/Mumunu-team/mununu/blob/main/deny.toml)).

## See also

- [Getting Started](Getting-Started) — installation + first example.
- [RTL Verification Pipeline](RTL-Verification-Pipeline) — where sv2v / Yosys / Verilator are used end-to-end.
- [Predicate-Cube CEGAR](Predicate-Cube-CEGAR) — where CVC5's Craig interpolation plugs in.
