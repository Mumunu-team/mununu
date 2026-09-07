# LinkedIn post — A TLS handshake with closed-IP AES — and what shared contracts buy you

> **Draft for LinkedIn.** Target: 150–200 words. Source artefact: `examples/industrial/tls_handshake/`. Do not publish until the four-gate validation checklist in `examples/industrial/tls_handshake/publications/README.md` passes.

---

Every commercial TLS termination device — smart NIC, HSM, edge-gateway accelerator — wires an open handshake state machine to a few closed-IP crypto blocks: AES, RNG, occasionally HMAC. Verifying the handshake requires *some* assumption about what those blocks do. Hand-writing a fresh contract for every project is wasted work, because AES-CTR is AES-CTR. NIST SP 800-38A is the same standard, regardless of vendor.

I wrote a worked example walking the architecture in every commercial TLS device. The AES core's wrapper carries a one-line annotation pointing at a shared library entry; the discovery pipeline resolves it and replaces the default chaotic stub with the vetted contract automatically. The default `output_sequencing` gap downgrades to `latency_bound` — what you'd gain locally is now a single missing clause, not a whole contract. The TRNG entry deliberately misses on purpose, demonstrating the structured diagnostic that tells you exactly what's missing.

Safety properties hold over all reachable composed states under chaotic crypto. A liveness verdict would still need a vendor-supplied latency-bound contract — the gap report says so.

Reproducible end-to-end: `./examples/industrial/tls_handshake/validate.sh`.
Full write-up: [Substack link TBD].
Code: github.com/Mumunu-team/mununu.

#formalverification #tls #hardware #verification
