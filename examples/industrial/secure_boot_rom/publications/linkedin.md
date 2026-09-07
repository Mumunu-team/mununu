# LinkedIn post — Verifying secure boot when you cannot see inside the crypto blocks

> **Draft for LinkedIn.** Target: 150–200 words. Source artefact: `examples/industrial/secure_boot_rom/`. Do not publish until the four-gate validation checklist in `docs/design/black-box-modules.md` §10.3 passes.

---

A secure boot ROM does a small job that has produced large incidents: `checkm8` on iPhones, the TrustZone bootloader bypasses, the IoT secure-boot failures. The job is "hash the firmware, verify the signature, unlock the bus or refuse." The hard part is that the SHA-256 and the RSA-verify cores arrive as closed IP — encrypted SystemVerilog, pre-synthesized macros, vendor wrappers. You can prove things about your boot controller only if you can describe how those black boxes behave, and only if your proofs don't quietly trust unsound assumptions.

I wrote a worked example walking the architecture used in OpenTitan, ARM TrustZone, and Google Titan M. The verifier handles each closed IP as a chaotic stub by default, surfaces the soundness consequence as a structured warning, and refuses to silently accept circular contract reasoning when you start authoring assume/guarantee clauses.

The property "no host-bus unlock without a completed verify" holds under chaotic crypto. The property "valid firmware eventually boots" needs a vendor latency contract — the gap report tells you exactly that.

Reproducible end-to-end: `./examples/industrial/secure_boot_rom/validate.sh`.
Full write-up: [Substack link TBD].
Code: github.com/Mumunu-team/mununu.

#formalverification #secureboot #hardware #verification
