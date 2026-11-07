# Secure boot ROM — black-box modules in a critical use case

> **Industrial example** for [Document A — black-box modules in compositional extraction](../../../docs/archive/design/black-box-modules.md) §9. Exercises every concept the document covers end-to-end against the mununu binary on `main`.

## What this example is

A secure boot ROM that verifies a firmware signature before allowing execution. The cryptographic primitives — a SHA-256 hash engine and an RSA signature-verify block — are modelled as **closed-IP black boxes**: the boot controller can drive their inputs and observe their outputs, but cannot see inside.

This is the architecture used in commercial secure-boot ROMs across the industry (Apple Secure Enclave, ARM TrustZone, Google Titan M, the OpenTitan reference design). It is mainstream, not academic.

```
┌─────────────────────────────────────────┐
│ Secure Boot ROM (open, verifiable)      │
│  ├─ BootController FSM                  │
│  ├─ Flash-read sequencing               │
│  └─ Host-bus arbiter                    │
└─────────────────────────────────────────┘
        │                  │
        ▼                  ▼
┌─────────────┐    ┌─────────────────┐
│ SHA-256 IP  │    │  RSA-verify IP  │
│ (Vendor V1) │    │  (Vendor V2)    │
└─────────────┘    └─────────────────┘
```

## What this example demonstrates

Every concept Document A introduces is exercised by `validate.sh`:

| Concept (Document A §) | How the example exercises it |
|---|---|
| Chaotic-stub default (§2) | `SHA256_V1` and `RSA_V2` automata have *uncontrollable* output transitions — they can stall forever, matching the chaotic-stub semantics. The boot controller's safety verdicts hold; its liveness verdicts would need vendor contracts. |
| Phase-1 discovery (§A5) | `mununu contract discover` on each crypto IP's interface JSON produces a controllability map + an `OutputSequencing` gap marker. |
| Gap-marker diagnostics + `--strict-contracts` (§2.iii, §A3) | The transcript shows `WARN contract gap detected …` lines with module / kind / labels / source location / soundness note. Strict mode exits non-zero. |
| Controllability rule (§4) | Each IP's inputs are classified `Uncontrollable` from the boot controller's perspective (they are the IP's view), and each IP's outputs are classified `Uncontrollable` from the boot controller's perspective (the IP drives them — the surrounding logic cannot predict the value). The Document A §4 rule fires twice with different conclusions per side. |
| Discharge check (§3.x, §A2) | Three contract sets demonstrate the four verdict kinds the discharge analyser can produce: `acyclic` (proper composition), `circular` (deliberately broken variant), `circular with mu-rank witness` (same cycle plus mu-rank annotations that satisfy the lightweight McMillan check). |
| Lightweight McMillan rank witness (§3.x, §A8) | The rank-witnessed contract set carries `mu_rank` values that strictly descend around the cycle except at one base edge; mununu auto-accepts. |

## What this example does *not* claim

Per the [CLAUDE.md claims-integrity rules](../../../CLAUDE.md), the doc is explicit about what is *not* being asserted:

- It does **not** claim mununu found a vulnerability in any commercial secure-boot ROM. The vendor-IP black boxes are stylised; the boot controller is hand-authored for the demonstration.
- It does **not** claim the closed-IP contract clauses are accurate to real-world SHA-256 or RSA implementations. They are illustrative of the *contract shape*, not derived from any specific vendor's datasheet.
- It does **not** prove that any real device using this architecture is secure. The proof is conditional on the contracts; the contracts are conditional on the vendor honouring its datasheet.
- It does **not** exercise vendor `@mununu_guarantee` source-comment annotations (those land in task A6 / milestone M3) or contract-corpus lookups (Document D). Until M3 lands, mununu emits chaotic stubs with full diagnostic visibility — the right safety posture.

## How to run it

From the repo root:

```bash
./examples/industrial/secure_boot_rom/validate.sh
```

The script builds the `mununu` binary, runs every command exercised in the demonstration, strips per-run noise (timestamps + ANSI escapes), and writes a byte-deterministic transcript to `transcript.txt`.

Re-running `validate.sh` against the same commit produces an identical `transcript.txt`. This is the evidence cited in the accompanying Substack and LinkedIn posts.

### What `validate.sh` does, step by step

1. **`mununu contract discover sha256_interface.json`** — phase-1 discovery on the SHA-256 IP. Produces six labels (clk / reset_n / start_hash / data_in are `Uncontrollable`; hash_valid / hash_out are `Uncontrollable` from the boot controller's side, then `Controllable` from inside the chaotic stub — note the rule applies per-scope) plus one `OutputSequencing` gap marker.
2. **`mununu contract discover rsa_verify_interface.json --emit-fairness-gap`** — same on the RSA IP, plus a `Fairness` gap to demonstrate the opt-in marker for liveness assumptions.
3. **`mununu contract validate contract_set_acyclic.json`** — the *correct* discharge graph for the composition. Verdict: `acyclic`. Topological order surfaced.
4. **`mununu contract validate contract_set_circular.json`** — a *deliberately broken* variant where the boot controller's pubkey-loaded guarantee depends on RSA's verify_ok beforehand. Verdict: `circular reasoning required (no mu-rank witness)`. Mununu refuses to silently accept.
5. **`mununu contract validate contract_set_rank_witness.json`** — same cycle as (4), but every clause now carries a `mu_rank`. Walking the cycle yields three strict descents and one base edge. Verdict: `circular with mu-rank witness (auto-accepted, McMillan-style)`.
6. **`mununu contract discover sha256_interface.json --strict-contracts`** — same discovery as (1) but with strict mode. Exits non-zero because the gap marker is unmet.
7. **Three `mununu context eval` runs** verify three properties against the composed CTXDSL model:
   - Safety (no execution without verified signature) → holds in 1/1 reachable composed states.
   - Reachability (BootValid reachable from Reset) → holds in 8/8 boot-controller states.
   - Reachability (Reset reachable from any state — reboot smoke test) → holds in 8/8.

The expected transcript is checked into `transcript.txt`.

## Files

| File | Purpose |
|---|---|
| `secure_boot.ctxdsl` | The hand-authored composition: BootController (open) + SHA256_V1 (chaotic stub) + RSA_V2 (chaotic stub), with three mu-calculus formulas. |
| `sha256_interface.json` | Black-box interface description fed to `mununu contract discover`. |
| `rsa_verify_interface.json` | Same shape for the RSA IP. |
| `contract_set_acyclic.json` | A discharge graph that the Pnueli 1985 rule covers. |
| `contract_set_circular.json` | A discharge graph that contains a cycle and no rank annotations. Mununu refuses to silently accept. |
| `contract_set_rank_witness.json` | Same cycle as the circular variant, but with `mu_rank` values satisfying the lightweight McMillan check. Auto-accepted. |
| `validate.sh` | Reproduces the transcript end-to-end. |
| `transcript.txt` | The byte-deterministic transcript `validate.sh` produces; cited as evidence. |

## Provenance

This example was authored for Document A's industrial demonstration. It is not derived from any specific commercial implementation. All vendor identifiers (`V1`, `V2`) are placeholders; no real silicon was modelled.
