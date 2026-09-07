# Verifying a secure boot ROM you cannot see all the way through

> **Draft for Substack publication.** Source: `examples/industrial/secure_boot_rom/`. The transcript embedded below reproduces byte-for-byte by running `./examples/industrial/secure_boot_rom/validate.sh` against the pinned commit. **Do not publish until the four-gate validation checklist in `docs/design/black-box-modules.md` §10.3 passes.**

---

A secure boot ROM does a job that sounds simple until you write it down. Power comes up, the ROM loads a firmware image from flash, hashes it, verifies the signature against a stored public key, and either unlocks the host bus (firmware is authentic) or refuses to boot (firmware was tampered with). A bug here is catastrophic. `checkm8` on iPhones, the secure-boot bypasses in various IoT devices, the various TrustZone bootloader incidents over the years — real firmware signature checks have been bypassed by exactly the kind of property a model checker is designed to catch.

The architecture is mainstream. The reference shape used in OpenTitan, Apple's Secure Enclave, ARM TrustZone bootloaders, and Google's Titan M looks like this:

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

The boot controller is yours; the crypto blocks are someone else's. The vendors ship them as encrypted SystemVerilog, pre-synthesized macros, or `(* blackbox *)` netlists. You have the datasheet and the port list. You do not have the body.

You want to prove three things about this design:

1. **Safety.** The host bus is never unlocked unless we previously completed a successful verify.
2. **Confidentiality.** Key material does not appear on the host bus during the verify operation.
3. **Liveness.** A valid firmware eventually boots.

(1) and (2) are safety properties. (3) is a liveness property. A formal verifier sees only what it can see. If it cannot see inside the SHA core or the RSA-verify core, it cannot prove a property that depends on their internals. The honest move is to verify the surrounding controller logic against a *contract* describing how each closed IP behaves. If the contract is right, the proof transfers to the real device. If the contract is wrong, the proof is meaningless.

This post walks the whole story end-to-end against the example at [`examples/industrial/secure_boot_rom/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/secure_boot_rom). You can reproduce every command. The transcript below is byte-deterministic — run `validate.sh` against the same commit and you will get the same output character-for-character.

## The conservative default — a chaotic stub

Start with the SHA-256 engine. You have its interface: a clock, a reset, a `start_hash` input, a `data_in` bus, a `hash_valid` output, a `hash_out` bus. You do not have its body. What is the most general environment that respects this interface?

The textbook answer comes from Kupferman, Vardi, and Wolper's *Module Checking* (J. ACM 47(2), 2000): treat the module as a single state that nondeterministically produces any output its interface admits, any time. Pessimism is mandatory — under-approximation would let the verifier miss real behaviours.

In CTXDSL, mununu's input language, that looks like a tiny automaton:

```
automaton SHA256_V1 {
    states {
        state Idle initial;
        state Computing;
    }
    transitions {
        transition Idle -> Computing on label start_hash;
        transition Computing -> Computing on label sha_compute_tick;
        // Output: hash_valid rises → back to Idle. This may never
        // fire (chaotic — no bounded latency contract).
        transition Computing -> Idle on label hash_valid_rises;
    }
}
```

The crucial line is the last transition: `hash_valid_rises` *can* fire from `Computing`, but the verifier cannot assume it *will*. The `Computing → Computing` self-loop is always available. From the boot controller's perspective, the SHA engine might never finish.

Is this realistic? No — real SHA-256 hardware finishes in 64 cycles. Is it *sound*? Yes — every real behaviour is a subset of this chaotic one. Verdicts about the boot controller's safety transfer to the real device. Verdicts about its liveness do not, because the real device offers a progress guarantee the chaotic stub does not.

## Making the default visible

The risk with a chaotic stub is forgetting it is there. You write your boot controller, you verify your safety properties, you ship — and quietly, your verdicts assume "the SHA engine might never finish," which means your liveness claims were vacuous all along.

The example hands the verifier a description of the black-box interface — just the port list with directions:

```json
{
  "name": "SHA256_V1",
  "ports": [
    { "name": "clk",        "direction": "Input" },
    { "name": "reset_n",    "direction": "Input" },
    { "name": "start_hash", "direction": "Input" },
    { "name": "data_in",    "direction": "Input" },
    { "name": "hash_valid", "direction": "Output" },
    { "name": "hash_out",   "direction": "Output" }
  ],
  "source_file": "rtl/vendor/sha256_v1.sv",
  "source_line": 8
}
```

Running `mununu contract discover sha256_interface.json` against this returns:

```
WARN contract gap detected — chaotic stub default in effect
     module="SHA256_V1" kind=output_sequencing
     labels="hash_valid, hash_out"
     location="rtl/vendor/sha256_v1.sv:8"
     soundness="safety verdicts hold; liveness verdicts depending on
                these labels are unsound (no progress assumption)."
phase-1 discovery: 6 label(s), 1 gap marker(s) for module `SHA256_V1`
```

The warning is not optional. It fires every time. It names the module, the gap kind, the labels involved, the source location, and — critically — the *soundness consequence*: which verdicts are still trustworthy and which are not.

For CI in safety-critical projects, `--strict-contracts` turns the warning into a hard error. Either the user authors a contract closing the gap, or the build fails. That discipline is what turns a chaotic stub from a quiet liability into an honest baseline.

## When two vendor promises argue with each other

Once you start authoring contracts, you can write proofs that contradict themselves. This is a classic assume-guarantee pitfall: module A's assumption is discharged by module B's guarantee, B's guarantee is conditioned on B's assumption, B's assumption is discharged by A's guarantee, A's guarantee depends on A's assumption. The cycle closes; the proof appears to hold; it is unsound.

McMillan's *Circular Compositional Reasoning about Liveness* (CHARME 1999) tells you when this kind of reasoning is sound. There has to be a temporal well-founded ordering — a way to assign a rank to each clause such that the cycle decreases strictly at every step except one inductive base. Without that witness, the cycle is invalid.

The example deliberately authors an accidentally circular contract set:

```json
{
  "clauses": [
    { "id": "G_boot_pubkey_loaded", "kind": "guarantee",  "owner": "BootController" },
    { "id": "A_rsa_pubkey_loaded",  "kind": "assumption", "owner": "RSA_V2" },
    { "id": "G_rsa_verify_ok",      "kind": "guarantee",  "owner": "RSA_V2" },
    { "id": "A_boot_verify_ok",     "kind": "assumption", "owner": "BootController" }
  ],
  "discharges": [
    { "discharger": "G_boot_pubkey_loaded", "dischargee": "A_rsa_pubkey_loaded" },
    { "discharger": "A_rsa_pubkey_loaded",  "dischargee": "G_rsa_verify_ok" },
    { "discharger": "G_rsa_verify_ok",      "dischargee": "A_boot_verify_ok" },
    { "discharger": "A_boot_verify_ok",     "dischargee": "G_boot_pubkey_loaded" }
  ]
}
```

The boot controller's `pubkey_loaded` guarantee depends on RSA's `verify_ok` guarantee, which depends on the boot controller's `verify_ok` assumption, which depends back on `pubkey_loaded`. Running `mununu contract validate` over the discharge graph emits:

```
discharge: circular reasoning required (no mu-rank witness)
  cycles:
    - [G_boot_pubkey_loaded -> A_rsa_pubkey_loaded -> G_rsa_verify_ok -> A_boot_verify_ok]
  → mununu refuses to silently accept circular discharge.
    HITL must approve, or one cycle clause must be rewritten
    to be unconditional.
  Tip: assign `mu_rank` to each clause for the lightweight
       McMillan-style automatic discharge (task A8).
```

This is the kind of bug that lives in design reviews for years — the dependency between three datasheet promises that nobody quite traces all the way around. The discharge graph catches it before verification even starts, courtesy of running Tarjan's strongly-connected-components algorithm over the `guarantor → consumer` edges.

## The lightweight rank witness

Sometimes the cycle is intentional and sound. Arbiter↔master fairness is a canonical case: each side's progress is contingent on the other's, and a well-founded induction over alternation depth makes the discharge work. The full McMillan rule requires step-indexed reasoning that the verifier does not currently implement.

What it *does* implement is a lightweight version: if every clause in the cycle carries a `mu_rank` annotation, and the ranks strictly descend around the cycle except at one base edge, the cycle is auto-discharged with provenance tag `mununu-verified circular discharge (mu-rank)`.

Add ranks to the same cycle:

```json
{ "id": "G_boot_pubkey_loaded", "kind": "guarantee",  "owner": "BootController", "mu_rank": 4 },
{ "id": "A_rsa_pubkey_loaded",  "kind": "assumption", "owner": "RSA_V2",         "mu_rank": 3 },
{ "id": "G_rsa_verify_ok",      "kind": "guarantee",  "owner": "RSA_V2",         "mu_rank": 2 },
{ "id": "A_boot_verify_ok",     "kind": "assumption", "owner": "BootController", "mu_rank": 1 }
```

Walking the cycle: 4 → 3 → 2 → 1 → 4. Three strict descents, one base edge. The validator accepts:

```
discharge: circular with mu-rank witness (auto-accepted, McMillan-style)
  - cycle [G_boot_pubkey_loaded -> A_rsa_pubkey_loaded -> G_rsa_verify_ok -> A_boot_verify_ok],
    base edge: A_boot_verify_ok -> G_boot_pubkey_loaded
```

This is intentionally conservative. It catches the cases where the rank ordering is obvious. For cycles that require deeper temporal reasoning, the validator falls back to "user-approved circular discharge" — the human takes responsibility, with the provenance tag making the assumption auditable later.

## The properties, verified

With the contract machinery in place, the actual verification is uneventful. Three properties over the boot controller automaton:

```
safety_no_execution_without_signature  →  SAT (1/1 reachable states)
bootvalid_reachable                    →  SAT (BootValid reachable from Reset)
reset_always_reachable                 →  SAT (Reset reachable from any state)
```

The safety verdict holds *under chaotic crypto*. That is the substantive claim. A real secure boot ROM whose surrounding logic looked like this would inherit the safety property regardless of what the SHA and RSA engines did internally — as long as they conform to the interface alphabet, which any conforming hardware would.

The liveness verdict ("eventually boots") is the one that does not transfer. Under chaotic crypto, the SHA engine might never complete, so the boot might never finish. To prove liveness, you would have to author a vendor-supplied `latency_bound` contract — the kind of clause that the discovery pipeline's gap marker explicitly invited you to add.

## What this example does not claim

Per [mununu's claims-integrity rules](https://github.com/Mumunu-team/mununu/blob/main/CLAUDE.md), the example is explicit about its boundaries:

- It does not claim mununu found a vulnerability in any commercial secure-boot ROM. The vendor-IP black boxes are stylised; the boot controller is hand-authored for the demonstration.
- It does not claim the closed-IP contract clauses are accurate to any vendor's real datasheet. They are illustrative of the *contract shape*.
- It does not prove a real device is secure. The proof is conditional on the contracts; the contracts are conditional on the vendors honouring their datasheets.
- This particular example does not exercise vendor source-comment annotations (`@mununu_guarantee` on the wrapper module) or contract-corpus lookups. Those mechanisms ship in [`examples/industrial/tls_handshake/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/tls_handshake) — the next post in this series. The secure boot ROM example deliberately stays at the chaotic-stub baseline so the reader can see the unannotated default behaviour first.

What the example *does* claim: every concept introduced here — chaotic stub, gap markers, controllability rule, discharge graph, mu-rank witness — is exercised end-to-end against the mununu binary on `main`, with a byte-deterministic transcript anyone can reproduce.

## What this slots into

The vocabulary is borrowed. Chaotic-stub semantics come from Kupferman, Vardi, and Wolper's *Module Checking* (CAV '96 / J. ACM 47(2), 2000). Contract-shaped automata at module boundaries come from de Alfaro and Henzinger's *Interface Automata* (ESEC/FSE 2001). Assume-guarantee discharge comes from Pnueli's *In Transition from Global to Modular Temporal Reasoning about Programs* (NATO ASI 13, 1985) and Abadi and Lamport's *Conjoining Specifications* (ACM TOPLAS 17(3), 1995). The circular reasoning rule comes from McMillan's *Circular Compositional Reasoning about Liveness* (CHARME 1999). The controllability framing comes from Alur and Henzinger's *Reactive Modules* (FMSD 15(1), 1999). The whole compositional verification programme traces back to de Roever, Langmaack, and Pnueli's *Compositionality: The Significant Difference* (COMPOS '97, LNCS 1536).

What mununu (the [open-source compositional model checker](https://github.com/Mumunu-team/mununu) this example runs against) contributes is the *integration*: a single workflow that pulls all of these into a CLI you can run today against a real example, with the right diagnostics where they matter, and the discipline of refusing to silently accept anything unsound. The contract subsystem this post walks through — chaotic-stub defaults that always emit a structured gap report, an automatic discharge-graph analysis that catches circular reasoning, and the lightweight mu-rank witness that auto-accepts well-formed circular contracts — is the piece that landed this milestone.

## What's next

This is post 1 of a four-part series. The remaining three each lead with a different real-world architecture:

- **Post 2 — A two-pipeline SoC.** Why the same SoC can be verified at two precision tiers (protocol-level vs gate-level) without forcing the user to pick. Worked example: [`examples/industrial/dual_frontend_soc/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/dual_frontend_soc).
- **Post 3 — A TLS handshake driving closed-IP crypto.** When the closed IP is *not unique* (AES-CTR is AES-CTR), a shared corpus replaces per-project contract authoring. Worked example: [`examples/industrial/tls_handshake/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/tls_handshake).
- **Post 4 — A UART driver + UART peripheral.** Cross-boundary HW/SW codesign verification: firmware C + peripheral SV with a register-map sidecar gluing the two sides. Worked example: [`examples/industrial/codesign_uart/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/codesign_uart).

The repo: [github.com/Mumunu-team/mununu](https://github.com/Mumunu-team/mununu).
The example: [`examples/industrial/secure_boot_rom/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/secure_boot_rom).
The design doc: [`docs/design/black-box-modules.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/black-box-modules.md).

— Mariano Cerrutti
