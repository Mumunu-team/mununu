# Claims Integrity — Public Materials

> Status: policy (binding for any material that references real-world systems).

**Every public claim about mununu's ability to find bugs, verify properties, or improve security of external systems must be backed by reproducible evidence against real implementations.**

This applies to: README examples, wiki case studies, blog posts linked from the repo, conference papers, and any material that references real-world systems (MCP servers, protocol implementations, hardware designs).

CLAUDE.md carries a short summary; this file is the full policy. Read it before publishing.

---

## Rules

### 1. Models from source, not documentation

If a claim says "mununu found X in system Y," the CTXDSL model must be extracted from Y's actual source code via an auditable extraction spec (line-anchored JSON referencing exact commit + file + line numbers). Models written from API docs or design descriptions must be labeled as "design pattern demonstrations," never as findings about the real system.

### 2. Planted bugs are demos, not findings

A hand-written model with a deliberately introduced defect demonstrates the tool's verification capability. It does not demonstrate that the real system has the defect. Language must reflect this: "we demonstrate the property class" vs. "we found a bug."

### 3. Severity honesty

A missing guard that creates a theoretical race window in single-threaded Node.js is not the same as an exploitable RCE. Claims must distinguish:

- **Security vulnerability** (data leak, privilege escalation, RCE)
- **Reliability / correctness issue** (error during edge-case concurrency)
- **Structural gap** (missing guard with no demonstrated impact)
- **Design pattern violation** (deviation from spec, not necessarily a bug)

### 4. Reproduction path required

Every claimed finding must include either:

- A test case or scenario that triggers the behavior in the real implementation, **or**
- An honest statement that the finding is structural (present in the state machine) but not yet reproduced against the running system.

### 5. Abstraction soundness

When abstracting from a real implementation (e.g., collapsing a `Map<K,V>` to a 3-state enum, or bounding an integer counter to a small domain):

- Every abstraction must be documented in the extraction spec with **what** was abstracted, **what was lost**, and **why** it is sound for the properties being checked.
- Over-approximation (model admits more behaviors than reality) is conservative for safety properties. Under-approximation (model admits fewer behaviors) is unsound — state this explicitly if it applies.
- After verification, **at least one concrete execution trace** must exercise the abstracted path against the real implementation (test case, curl command, or script). If the model says a property fails, demonstrate the violation is reproducible on the real system — not an artifact of the abstraction. If the model says it holds, demonstrate a representative concrete case passes.
- Abstractions that cannot be validated by a concrete scenario must be flagged and must not support public claims.

### 6. Verification-first workflow

When analyzing external codebases for vulnerabilities, the CTXDSL model and `mununu` verification is the **oracle** — not human reasoning about source code. The workflow is:

```
extract → generate CTXDSL → run `mununu context eval` on the composition → interpret the result
```

Do not pre-conclude whether a property holds or fails based on code reading. The tool explores all reachable states and finds traces humans miss. Build the model, run the check, let the tool speak. Only after the formal result, validate against the real implementation.

### 7. Extraction pipeline for claims about real systems

The pipeline is:

```
Source code → Extraction spec (JSON) → validate_extraction_spec.py → spec_to_ctxdsl.py → mununu eval/synth
```

The extraction spec is the auditable artifact. It must be committed alongside the claim. The spec validator must pass against the pinned commit.

### 8. Editorial framing for publications

Long-form public content (LinkedIn, Substack, blog posts, conference talks) must lead with a realistic, impactful example — a concrete system, a concrete property, and a concrete consequence — and treat mununu features as secondary, introduced only where they directly serve the example.

- **Lede is the example, not the tool.** The headline, opening paragraph, and pull quotes must describe the system and what could go wrong with it (a UART controller drops a byte under back-pressure; a payment FSM admits a double-spend interleaving; an MCP server leaks a session across tenants). They must not headline a mununu feature ("we shipped a new coupling-synthesis pass", "our register-map sidecar now supports …"). Feature launches belong in release notes or technical READMEs, not in public storytelling pieces.
- **Features appear when they earn the right.** A capability (a new adapter, a new template, a new CLI flag, a new emit mode) may only be named when (a) it is what made the example tractable, **or** (b) it changes how the reader would reproduce the result. It is introduced inline, *after* the example has been set up, and tied back to a code anchor per the Documentation Traceability rule.
- **No feature catalogs.** A bulleted list of "what's new" without an example attached to each bullet is rejected. If the post genuinely needs to enumerate features, those features must be grouped under the example each one supported, not under the release.
- **Impact must be concrete.** "Realistic and impactful" means the example references real systems with public source-of-truth (per Rule 1), or — for Class C demonstrations — clearly labels itself as a pattern study with named real-world analogs. Synthetic toy automata ("a 3-state safety property") do not qualify as the lede of a public post.
- **One example per post, by default.** A LinkedIn or Substack post lives or dies on a single clear story. Multi-example posts are allowed only when the examples share a single, named thread (e.g., "three controller-synthesis examples in industrial UART variants").
- **Applies to**: LinkedIn posts, Substack / Medium articles, conference talks, demo videos, podcast prep notes, and any agent-generated content suggestion that targets these channels. Does **not** apply to internal release notes, CHANGELOG entries, wiki reference pages, or API documentation, which may lead with the feature.

### 9. RTL / SystemVerilog pipeline evidence integrity

The same verification-first principles apply to the SV Kripke pipeline:

- **Never present hand-written data as pipeline output.** If `discovered_values` in a `.mununu.json` sidecar were written by a human or AI agent (not by `mununu btor2 discover`), they must be disclosed as hand-written. Claims like "SMT discovers x=3" require actually running the discover command.
- **Run the pipeline, don't simulate it.** Before presenting verification results in any public material, execute the actual commands and capture real terminal output. Do not fabricate or predict mununu output.
- **Properties must come from specifications, not bug knowledge.** Adding a detector register to catch a known bug and then verifying it fires is circular. Properties should come from protocol specs, safety invariants, or security requirements.
- **Distinguish syntactic from SMT-discovered values.** Literals found directly in `case` labels are syntactic. Values found through combinational logic inversion are SMT-discovered. Don't claim SMT discovery for trivially visible constants.
- **Show counterexample traces.** When a property fails, capture the violating state / transition trace, not just "unrealizable."
- **Validate the trace under simulation.** Whenever an RTL diagnostic (counterexample or counterstrategy from `mununu context synth --counterexample`) is used to support a public claim, the trace must be reproduced against the close-to-source SystemVerilog under Verilator, in the sibling `hw-verif:latest` Docker image. The reproduction lives in `staging/{target_id}/repro/` with a `.sv` per case-modifier variant relevant to the bug, a Verilator C++ testbench that drives the trace inputs (using `force` for fault hypotheses), and a `Makefile` with a `sim` verb. The simulation transcript is the evidence; a model-only lasso / counterexample is labeled "LTS witness only — not reproduced in simulation" and downgrades the rigor by one level. See the `target-executor` agent's Phase 3.5 for the procedure and `examples/verify/sv_yosys_caliptra_rtl_150/` for the canonical pattern.

These rules apply to both human and AI-agent authored content.

### 10. C extractor — hand-authored IR fixtures vs end-to-end real-clang extraction

The codesign C extractor at [`crates/mununu-core/src/codesign/c_extract_llvm.rs`](../../crates/mununu-core/src/codesign/c_extract_llvm.rs) has two kinds of validation: unit tests on hand-authored IR text fragments, and end-to-end runs against real C source compiled by real clang. These are **different claims** and must be reported as such.

- **A unit test that constructs an IR fragment in a Rust string literal demonstrates the matcher's behaviour on that IR shape.** It does NOT demonstrate that clang produces that IR shape for the corresponding C source — the test author wrote the IR by hand based on what they *believed* clang would emit. Treat hand-authored IR fixtures as documenting the extractor's *behaviour on a given input*, never as evidence that the extractor handles a particular C idiom end-to-end.
- **The claim "the extractor handles `<C-idiom>`" requires a real `.c` file** committed under `examples/industrial/codesign_uart/motivating_examples/` (or similar) that (a) compiles via `clang -O0 -emit-llvm -S`, (b) is consumed by `mununu codesign extract-c`, and (c) produces the claimed access structure. A `validate-motivating-examples.sh`-style script that exercises every claimed example end-to-end is the canonical evidence. PRs that introduce a new extractor capability MUST add the corresponding real example or honestly downgrade the claim to "the matcher handles this IR shape; the corresponding clang-emitted IR has not been validated."
- **Honest gap statements are required.** Phase L5's "interprocedural call-graph walk" closes Example 4 only for callees that touch globals directly. Pointer-aliased callees (alloca-store-load round-trip) are queued as L5.5 — that gap must be stated, not glossed over. The Doc C correctness-scope note at [`docs/design/c-extraction-correctness-scope.md`](../design/c-extraction-correctness-scope.md) is the canonical home for these statements.
- **Hand-authored IR fixtures stay valuable** for fast unit tests of single regex shapes / single matcher branches. They must just be labelled in the test name or doc-comment as "synthetic IR" and not cited in PR descriptions or publications as evidence of end-to-end handling of any C idiom.

---

## What this does NOT restrict

- Tutorial and pedagogical examples can use hand-written CTXDSL freely.
- Benchmark models (SYNTCOMP, protocol verification) follow their own established methodology.
- The adapter test suites use synthetic inputs by design.
- Academic papers may present idealized models if clearly labeled as such in the methodology section.
- Example `.mununu.json` sidecars with hand-written `discovered_values` are acceptable for tutorials **if** disclosed as hand-written in the accompanying documentation.
