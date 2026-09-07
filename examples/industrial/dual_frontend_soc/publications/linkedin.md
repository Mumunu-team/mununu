# LinkedIn post — A tiny SoC with closed-IP DDR and two verification questions

> **Draft for LinkedIn.** Target: 150–200 words. Source artefact: `examples/industrial/dual_frontend_soc/`. Do not publish until the four-gate validation checklist in `publications/README.md` passes.

---

A tiny SoC: an open host controller, an open UART, a closed-IP DDR3 PHY behind `(* blackbox *)`. Two verification questions land on the same SoC every quarter. *Does the burst path ever deadlock?* — that one wants protocol-level state with registers as enums. *Does the carry chain overflow in this corner case?* — that one wants gate-level bit-blasting. You should not have to pick.

I wrote a worked example that runs both questions against the same CTXDSL definition. The closed-IP DDR PHY becomes a chaotic stub by default; the verifier auto-emits the `BlackBoxInterface` + `GapMarkerReport` sidecars when the extractor sees the `(* blackbox *)` attribute, so the contract workflow no longer has to be hand-fed what the extractor already parsed. The properties "burst path reachable" and "UART send reachable" hold under chaotic DDR; a liveness property would need a vendor latency contract — the gap report says so explicitly.

The principle: **unify the seams, leave the cores free.** One IR, one composition primitive, one controllability rule — but two precision tiers, deliberately divergent. The CIRCT pattern, retrofitted onto an existing two-pipeline codebase.

Reproducible end-to-end: `./examples/industrial/dual_frontend_soc/validate.sh`.
Full write-up: [Substack link TBD].
Code: github.com/Mumunu-team/mununu.

#formalverification #rtl #systemverilog #hardware #verification
