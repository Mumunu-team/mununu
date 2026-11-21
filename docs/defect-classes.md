# Defect classes

> Source of truth: [`scripts/pr_review_gate.py`](../scripts/pr_review_gate.py) (`CATALOG_HEADING`: a fix's `Class: <slug>` must name a `###` heading of this file) — surface: CLI-only — a contributor instrument

The catalogue behind [`policies/fix-the-class.md`](policies/fix-the-class.md). One entry per class: **what varies**, the **cases**, what the code does on each, the **test that enumerates them**, and the fixes that have landed in the class. A fix in a class updates its entry; a fix that meets an unlisted case adds it.

The first seven entries were seeded on 2026-11-20 from the fix chains of the two weeks before. Their case tables record what those fixes covered. **They have not yet been checked against the code that produces the cases** — the first fix in each class does that and removes this caveat from the entry.

An entry's heading is `### ` followed by the slug in backticks; the gate reads exactly that.

---

### `free-cell-in-atom-cone`

**What varies:** the kind of free (next-less) cell a property atom's cone of influence reaches in the lifted BTOR2. The exact engine must read such a cell once per step; read once in the atom and again in the transition, the verdict is neither the may- nor the must-reading of the design.

| Case | Behaviour | Landed |
|---|---|---|
| a primary input | sampled by the inputs-in-the-state lift | mununu#677 |
| a named next-less state (a `--cutpoint` net) | sampled like an input | mununu#1062 |
| a next-less state with no symbol and no alias | sampled under its synthesized name | mununu#1086 |
| a synthesized name that collides with a design cell | the atom is refused, the collision named | mununu#1086 follow-up |
| any of the above with inputs-in-the-state opted out | the atom is refused by the fan-in guard | mununu#476 |

**Enumerating test:** none recorded. **Open:** unqualified existential `holds` over a clock-as-data or undriven net (mununu#1126).

### `clock-net-driver`

**What varies:** what drives a net a flop or memory port is clocked by, after the walk through inverters and buffers — read off the netlist the lift's script writes after `delete =A:mununu_missing_module; cutpoint -blackbox; flatten; opt_clean` (`ClockDrivers::root_of` → `ClockRoot` in `adapter/yosys/mod.rs`). The BTOR2 model has one implicit clock, so every root but a top-level input is an abstraction decision; the repository's rule for the lockstep reading is an explicit opt-in (`--merge-clock-domains` on `sv verify-auto`), refused elsewhere. The two black-box shapes were measured in `mununu-sva` on 2026-11-20 (the fixtures of the enumerating tests are those captures; the e2e re-lifts both).

| Case | Behaviour | Landed |
|---|---|---|
| a top-level input, or an `inout` port | the root clock | mununu#1148 (`inout`) |
| a `$`-cell (a gating `$and`, a divider flop) | a derived domain, refused unless `--merge-clock-domains` | mununu#758 |
| `$anyseq` from `cutpoint -blackbox` (a declared or empty black-box module: a PLL, a clock gate with no body) or from `--cutpoint` on the net | a cut net: refused by default naming the root (was refused as "derived from logic"; the `--cutpoint` case was accepted); under the opt-in it lifts with the `clock-rooted-at-black-box` note | mununu#1148 |
| a non-`$` cell with no `port_directions` (a `(* mununu_missing_module *)` stub deleted after the snapshot) | a black box: refused by default naming the cell (was a silent root — the case of mununu#1148); under the opt-in it lifts with the same note | mununu#1148 |
| a non-`$` cell with `port_directions` (a hand-written fixture; no lift path found that produces it) | a black box, as above | mununu#1148 |
| the other edge of a net already used as a clock | its own domain | mununu#758 |
| an undriven net | refused by default (`undriven`; was a silent root); under the opt-in the `clock-undriven` note | mununu#1148 |
| a constant clock bit | open: not a domain, silent | mununu#1180 |
| a net only on stub port lists; an undriven net that also feeds a stub input | open: attributed to the last stub inserted | mununu#1180 |
| an `inout` port of a black box | open: reads as undriven | mununu#1180 |
| a bit with two drivers | open: the last cell inserted wins | mununu#1180 |
| which verbs carry the note | `sv verify-auto` under the opt-in; every other verb refuses and has no switch (said in the refusal) | mununu#1148 |

**Enumerating test:** `every_clock_root_kind_is_classified_and_described` (exhaustive match over `ClockRoot`, one net per kind, the `inout` port included) and `the_two_black_box_shapes_are_free_roots_not_derived_domains` (the captured shapes: refused by default, accepted under the opt-in); the note's seam: `x1148_a_free_source_clock_is_a_soundness_caveat_with_its_cell`; the real lift on both shapes: `e2e_a_black_box_clock_is_refused_unless_merged_and_its_root_is_classified` (image).

### `initial-cube-doubt`

**What varies:** why the predicate-cube engine kept a candidate initial cube it could not prove infeasible at cycle 0. Keeping it is right for `holds` and wrong for `violated`.

| Case | Behaviour | Landed |
|---|---|---|
| the cycle-0 query was inconclusive | a `violated` read only there is withheld; host-dependent | mununu#1085 |
| the run budget was spent | withheld; host-dependent | mununu#1085 |
| a design `init` did not encode | withheld; reproducible | mununu#1061, mununu#1085 |
| a memory `init` did not encode | withheld; outranks an inconclusive query; remedy per cause | mununu#1085, mununu#1106 |
| the candidate set proved empty | withheld; its own remedy | mununu#1085 |

**Enumerating test:** none recorded.

### `member-stop-reason`

**What varies:** why a reach-portfolio member returned no verdict. Each reason must reach the report under its own name, with the member's progress.

| Case | Behaviour | Landed |
|---|---|---|
| a peer decided first | in no list | — |
| the member reached its own bound | `undecided`, with depth or frame, and the wall it left unused | mununu#1022, mununu#1038 |
| the wall or the run budget stopped it | `abstained`, with progress | mununu#1043 |
| the member refused the design | `refused` | — |
| the member panicked | open: unrecorded, or reported as still running | mununu#1143 |
| a rung of the justice / response ladder panicked | open | mununu#1140 |

**Enumerating test:** none recorded.

### `server-side-file-read`

**What varies:** how a request to a `[server] file_root` server can name a file the server then reads, or a program it then runs.

| Case | Behaviour | Landed |
|---|---|---|
| a path field of the request | confined to the root | mununu#935 |
| a path written inside a verify config | confined against the canonical base directory and the root | mununu#935 |
| an executable named by the request | refused | mununu#935 |
| an `#include` in a C source or a header it reaches | scanned; an absolute, `..` or macro-built target is refused | mununu#994 |
| a top module, parameter or source name handed to the lift script | validated before it reaches the script | mununu#1133 |

**Enumerating test:** none recorded. **Open:** there is no single function every server-side read goes through.

### `os-dependent-test-expectation`

**What varies:** what a test assumes about the host that only the Linux gate satisfies.

| Case | Behaviour | Landed |
|---|---|---|
| a path separator or a Unix path in an expected string | build the expected path with the same join the code uses | mununu#999, mununu#1153 |
| a wall-clock bound on a race between portfolio members | assert the verdict on every host and pin the member at a seam | mununu#998, mununu#1054, mununu#1065 |

**Enumerating test:** none; the OS matrix on `main` is the detector.

### `prose-anchored-test`

**What varies:** which words of a user-facing message a test depends on. A rewording that keeps the meaning breaks the test; one that changes the meaning but keeps the words passes.

| Case | Behaviour | Landed |
|---|---|---|
| a test anchors on a phrase of a remedy | the phrase was kept by hand | mununu#1127 |

**Enumerating test:** none recorded. **Open:** no list of the tests that anchor on prose.

### `trace-minimality-across-members`

**What varies:** which member's counterexample ships, and whether that member's search returns the shortest one.

| Case | Behaviour | Landed |
|---|---|---|
| a member that searches depth by depth | the trace is the shortest; the report says so | mununu#1080 |
| the SAT BMC dynamic step (one query per window) | the trace is the first found; the report says how far below it was refuted | mununu#1057, mununu#1080 |

**Enumerating test:** none recorded. **Open:** constraints asserted past the bad frame in the dynamic step (mununu#1141).
