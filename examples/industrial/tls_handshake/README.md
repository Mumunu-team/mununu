# TLS handshake — annotations + corpus in a critical use case

> **Industrial example** for [Document D — contract corpus + unified config](../../../docs/archive/design/contract-corpus-and-config.md) §D.9. Closes the M3 arc by exercising the full **annotation → corpus → gap-downgrade** chain end-to-end against the mununu binary on `main`.

## What this example is

A TLS handshake controller wired to two closed-IP cryptographic primitives:

```
┌───────────────────────────────────────────────┐
│ TLS Handshake (open, fully verifiable)        │
│  ├─ ClientHello → ServerHello → KeyDeriv      │
│  ├─ NonceReq → CipherReady → Finished         │
│  └─ Application loop (records over AES-CTR)   │
└───────────────────────────────────────────────┘
        │                       │
        ▼                       ▼
┌──────────────────┐    ┌──────────────────┐
│ AES-CTR core     │    │ TRNG (random)    │
│ (Vendor v1)      │    │ (Vendor V2)      │
│                  │    │                  │
│ @mununu_blackbox │    │ @mununu_blackbox │
│ @mununu_interface│    │ @mununu_interface│
│   contract://    │    │   contract://    │
│   rtl_crypto/    │    │   rtl_crypto/    │
│   aes_ctr@1.0.0  │    │   trng@2.0.0     │
│   ?alt=strict_iv │    │   (NOT IN CORPUS)│
└──────────────────┘    └──────────────────┘
       │                        │
       ▼                        ▼
  corpus hit              corpus miss
  → LatencyBound         → OutputSequencing
   (downgraded)            (default kept)
```

This shape — open controller + several closed crypto IPs — is the architecture used in every commercial TLS termination device shipped in the last decade (smart NICs, hardware security modules, embedded TLS accelerators). It is mainstream, not academic.

## What this example demonstrates

Every concept Document D introduces is exercised by `validate.sh`:

| Concept (Document D §) | How the example exercises it |
|---|---|
| Source-comment annotation grammar (§D.5) | Both interface JSONs carry `annotations[]` arrays with `@mununu_blackbox`, `@mununu_interface`, and `@mununu_guarantee` tags — the same shape `extract_from_sv_source` / `extract_from_yosys_attributes` would emit from real SystemVerilog. |
| Contract corpus (§D.2) | `corpus/rtl_crypto/aes_ctr@1.0.0.json` is a hand-authored AES-CTR contract with two named alternatives (`strict_iv`, `permissive`) and `mununu_verified` provenance. The example queries it directly *and* indirectly through annotation resolution. |
| `contract://` URI grammar (§D.5 + §D.2) | The AES interface's `@mununu_interface` annotation carries `contract://rtl_crypto/aes_ctr@1.0.0?alt=strict_iv` — domain, name, pinned version, and alternative all in one URI, parsed by the new `contract_uri` module. |
| Corpus resolution outcomes (§D.2 + A6) | Three of the four `CorpusResolution` statuses are exercised: **Resolved** (AES with `--corpus`), **NotFound** (TRNG with `--corpus`), and **NoCorpus** (AES without `--corpus`). The fourth (`Malformed`) is covered by the unit tests in `contract_uri`. |
| Phase-2 gap downgrade (Document A §A6) | The AES discovery output shows the default `OutputSequencing` gap downgraded to `LatencyBound` — equivalent in effect to discovering a guarantee clause in source, because a `Resolved` corpus hit ships a vetted automaton + formulas. |
| Three-surface parity (CLAUDE.md) | `--corpus <DIR>` exists on CLI (`contract discover`, `contract sidecars`), on HTTP (`POST /api/v1/contract/discover` body field), and via the same `Phase1Output` shape consumed by the UI. |
| Chaotic-stub default + `--strict-contracts` (Document A §2.iii) | The strict-mode gate refuses to proceed when a residual `LatencyBound` gap remains, even *after* a corpus hit. The user must either author the latency bound locally or accept the gap. |
| Soundness asymmetry (Document A §2) | The composed safety property holds across all 4 reachable states under chaotic crypto — over-approx + safety = sound. A liveness property would *not* hold without the user authoring the residual latency-bound gap. |

## What this example does *not* claim

Per the [CLAUDE.md claims-integrity rules](../../../CLAUDE.md), the doc is explicit about what is *not* being asserted:

- It does **not** claim mununu found a vulnerability in any real TLS implementation. The vendor-IP black boxes are stylised; the handshake controller is hand-authored for the demonstration.
- It does **not** claim the AES-CTR corpus entry is a complete or accurate model of any specific commercial AES core. The entry is explicitly tagged `illustrative — no real vendor silicon was modelled` in its provenance.
- It does **not** prove that any real TLS device using this architecture is secure. The proof is conditional on the contracts; the contracts are conditional on the vendor honouring the cited standard (NIST SP 800-38A for AES-CTR).
- It does **not** ship a TRNG corpus entry. The TRNG interface deliberately references `contract://rtl_crypto/trng@2.0.0` so the transcript exercises the `NotFound` resolution path, surfacing the missing-contract diagnostic. A real project would author a local `trng@2.0.0` entry next, then re-run discovery to clear the gap.

## How to run it

From the repo root:

```bash
./examples/industrial/tls_handshake/validate.sh
```

The script builds the `mununu` binary, runs every command exercised in the demonstration, strips per-run noise (timestamps + ANSI escapes), and writes a byte-deterministic transcript to `transcript.txt`.

Re-running `validate.sh` against the same commit produces an identical `transcript.txt`. This is the evidence cited in the accompanying Substack and LinkedIn posts.

### What `validate.sh` does, step by step

1. **`mununu contract query rtl_crypto/aes_ctr --corpus corpus`** — direct corpus lookup. Surfaces the entry's version, provenance (mununu-verified against NIST SP 800-38A), and description. Confirms the corpus is reachable before discovery uses it.
2. **`mununu contract discover aes_ctr_interface.json --corpus corpus`** — phase-2 discovery on the AES IP. The interface's `@mununu_interface` URI resolves against the corpus; the resolution is reported (`resolved … [alt strict_iv ok]`) and the default `OutputSequencing` gap is downgraded to `LatencyBound`.
3. **`mununu contract discover rng_interface.json --corpus corpus`** — phase-2 discovery on the TRNG. The URI parses cleanly but the corpus has no `trng@2.0.0` entry → the resolution is reported as `not found` and the default `OutputSequencing` gap is preserved. The user sees they need to author a local contract.
4. **`mununu contract discover aes_ctr_interface.json`** (no `--corpus`) — same interface, but no corpus supplied. The URI annotation is preserved; the resolution is reported as `referenced but no corpus supplied (use --corpus)`. The user sees what they would gain by pointing at a corpus.
5. **`mununu contract discover … --strict-contracts`** — strict-mode gate. Exits non-zero because the `LatencyBound` gap remains unauthored even after the corpus hit. Demonstrates that a corpus hit is necessary but not sufficient — the user must still close the residual gap (here, by authoring a latency bound) before strict mode passes.
6. **Three `mununu context eval` runs** verify three properties against the composed CTXDSL model:
   - Safety (every reachable composed state respects the protocol) → holds across 4/4 reachable states.
   - Reachability (`Application` reachable from `Idle` in the handshake) → holds in 8/8 handshake states.
   - Reachability (`Idle` reachable from every state — teardown smoke test) → holds in 8/8.

The expected transcript is checked into `transcript.txt`.

## Files

| File | Purpose |
|---|---|
| `tls_handshake.ctxdsl` | The hand-authored composition: TLSHandshake (open, 8 states) + AES_CTR_v1 (chaotic stub, 2 states) + TRNG_V2 (chaotic stub, 2 states), with three mu-calculus formulas. |
| `aes_ctr_interface.json` | Black-box interface for the AES IP, with `@mununu_blackbox` + `@mununu_interface contract://…?alt=strict_iv` + `@mununu_guarantee` annotations. |
| `rng_interface.json` | Black-box interface for the TRNG, with a `@mununu_interface` URI that intentionally has no corresponding corpus entry. |
| `validate.sh` | Reproduces the transcript end-to-end. |
| `transcript.txt` | The byte-deterministic transcript `validate.sh` produces; cited as evidence. |

Plus a top-level corpus addition this example depends on:

| File | Purpose |
|---|---|
| `../../../corpus/rtl_crypto/aes_ctr@1.0.0.json` | The vetted AES-CTR contract entry the AES IP's annotation resolves against. Ships with two named alternatives and `mununu_verified` provenance. |

## Provenance

This example was authored for Document D's industrial demonstration. It is not derived from any specific commercial implementation. All vendor identifiers (`v1`, `V2`) are placeholders; no real silicon was modelled. The AES-CTR corpus entry references the NIST SP 800-38A counter-mode reference as its `verified_against` source — the entry is illustrative of the *contract shape*, not a complete formal model of the standard.

## Related

- **[secure_boot_rom](../secure_boot_rom/)** — M1.c industrial example. Predates corpus integration; uses chaotic-stub defaults plus the lightweight McMillan circular-reasoning check. Read it first if you want to understand how the gap markers behave *without* corpus resolution.
- **[dual_frontend_soc](../dual_frontend_soc/)** — M2.c industrial example. Demonstrates the same `BlackBoxInterface` shape emitted from both the custom-SV and yosys/BTOR2 frontends.
