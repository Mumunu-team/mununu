# Defect classes

> Source of truth: [`scripts/pr_review_gate.py`](../scripts/pr_review_gate.py) (`CATALOG_HEADING`: a fix's `Class: <slug>` must name a `###` heading of this file) — surface: CLI-only — a contributor instrument

The catalogue behind [`policies/fix-the-class.md`](policies/fix-the-class.md). One entry per class: **what varies**, the **cases**, what the code does on each, the **test that enumerates them**, and the fixes that have landed in the class. A fix in a class updates its entry; a fix that meets an unlisted case adds it.

The first seven entries were seeded on 2026-11-20 from the fix chains of the two weeks before. Their case tables record what those fixes covered. **They have not yet been checked against the code that produces the cases** — the first fix in each class does that and removes this caveat from the entry.

An entry's heading is `### ` followed by the slug in backticks; the gate reads exactly that.

---

### `abandoned-request-work`

**What varies:** the work an HTTP request started that can outlive its response once the request times out or the client goes away: the handler body on the blocking pool, the engine threads and per-property budgets under it, the subprocess children, a thread with no budget poll, an MCP tool call.

| Case | Behaviour | Landed |
|---|---|---|
| the blocking handler body | runs under a run budget whose cancel flag goes up when the future is dropped | mununu#1147 |
| engine threads and per-property budgets under it | children of the request's budget (`Budget::child(&current(), …)`), so they see the cancel; the fault-reach handler's budget made a child | mununu#1147 |
| a subprocess child (yosys, btormc, Pono, rIC3) | the helper polls the flag and kills the child, returning `Interrupted` with the reason | mununu#1147 |
| a solver query child (cvc5, MathSAT) | the same helper: the two runners had loops of their own that never read the run budget. The helper is the one place that spawns a member child today; the member-spawning trait is private to `adapter/mod.rs`, which keeps other crates out but not the adapter's own submodules, and no test enforces the single call site | mununu#1186 |
| the isolated `exact-symbolic` child (`MUNUNU_ISOLATE_ENGINES`) | open: waited on with no wall, no cancel flag and no run-budget kill (mununu#1196) | — |
| a thread with no budget poll inside one operation (an OxiDD apply) | open: stops between stages only (mununu#996) | — |
| an MCP tool call past `--request-timeout` | open: the call errors and the engine thread finishes on its own | — |

**Enumerating test:** none in one table; `x1147_a_dropped_request_cancels_its_blocking_work` and `x1147_a_cancelled_run_budget_kills_the_child_it_waits_on` pin the first and third cases, and the cancelled-budget row of `x1186_every_way_a_solver_child_stops_is_named` the fourth.

### `free-cell-in-atom-cone`

**What varies:** the kind of free (next-less) cell a property atom's cone of influence reaches in the lifted BTOR2. The exact engine must read such a cell once per step; read once in the atom and again in the transition, the verdict is neither the may- nor the must-reading of the design.

| Case | Behaviour | Landed |
|---|---|---|
| a primary input | sampled by the inputs-in-the-state lift | mununu#677 |
| a named next-less state (a `--cutpoint` net) | sampled like an input | mununu#1062 |
| a next-less state with no symbol and no alias | sampled under its synthesized name | mununu#1086 |
| a synthesized name that collides with a design cell | the atom is refused, the collision named | mununu#1086 follow-up |
| any of the above with inputs-in-the-state opted out | the atom is refused by the fan-in guard | mununu#476 |
| an undriven net, or a clock net read as data, in the cone of an existential or branching property (a cover, an `EF`, an `AG EF`) | the HOLDS is withheld: `existential-holds-over-free-leaves`, `existential-holds-over-clock-as-data` | mununu#1126 |
| a verdict recorded before that on such a cone (the corpus ledger's `usbdev_linkstate` row, an `AG EF` over a missing module's outputs) | the entry carries the real module, so the row is HOLDS on a model with no undriven net in the cone; the ledger's failure message carries the report's reason | mununu#1192 |

**Enumerating test:** none recorded. **Open:** none recorded.

### `clock-net-driver`

**What varies:** what drives a net a flop or memory port is clocked by, after the walk through inverters and buffers — read off the netlist the lift's script writes after `delete =A:mununu_missing_module; cutpoint -blackbox; flatten`, on a copy cleaned by an `opt_clean` that keeps every named net (`cleaned-netlist-coverage` below) (`ClockDrivers::root_of` → `ClockRoot` in `adapter/yosys/mod.rs`). The BTOR2 model has one implicit clock, so every root but a top-level input is an abstraction decision; the repository's rule for the lockstep reading is an explicit opt-in (`--merge-clock-domains` on the eight `sv` verbs that take it, API `lift.merge_clock_domains`), refused without it. The two black-box shapes were measured in `mununu-sva` on 2026-11-20 (the fixtures of the enumerating tests are those captures; the e2e re-lifts both). The shapes of mununu#1180 were measured there on 2026-11-22 (ten captured netlists with their hierarchy snapshots; the e2e re-lifts seven of the sources). A stub's port directions are not in the flattened netlist — yosys writes none for a cell of unknown type — so the lift's scan reads them from the hierarchy snapshot it takes before the stubs are deleted (`clock_domains_with_hierarchy`).

| Case | Behaviour | Landed |
|---|---|---|
| a top-level input, or an `inout` port | the root clock | mununu#1148 (`inout`) |
| a `$`-cell (a gating `$and`, a divider flop) | a derived domain, refused unless `--merge-clock-domains` | mununu#758 |
| `$anyseq` from `cutpoint -blackbox` (a declared or empty black-box module: a PLL, a clock gate with no body) or from `--cutpoint` on the net | a cut net: refused by default naming the root (was refused as "derived from logic" on `main`, the `--cutpoint` case included; it was accepted only at the second head of #1170); under the opt-in it lifts with the `clock-rooted-at-black-box` note | mununu#1148 |
| a non-`$` cell with no `port_directions` (a `(* mununu_missing_module *)` stub deleted after the snapshot) | a black box: refused by default naming the cell (was a silent root — the case of mununu#1148); under the opt-in it lifts with the same note | mununu#1148 |
| a non-`$` cell with `port_directions` (a hand-written fixture; no lift path found that produces it) | a black box, as above | mununu#1148 |
| the other edge of a net already used as a clock | its own domain | mununu#758 |
| an undriven net | refused by default (`undriven`; was a silent root); under the opt-in the `clock-undriven` note | mununu#1148 |
| a clock pin tied to a constant (`CLK: ["0"]` — a wire tied low, a sub-instance's clock port tied to `1'b1`) | a domain of its own, named `1'b0` / `1'b1` with one storage cell on it (the first by name): refused by default ("the clock of `q` is tied to constant 0, which has no edge"; it was not a domain at all, so cells with no clock edge lifted as if they ticked, with nothing said); under the opt-in the `clock-constant` note. `sv cdc-lint` names it in a note and says its walk reads those cells as unclocked | mununu#1180 |
| a net on the ports of `(* mununu_missing_module *)` stubs | the stub whose declared `output` the net is on is the root — a black box, as above; a stub that only reads the net is not a driver (the last stub in cell order was named, whichever way its port pointed) | mununu#1180 |
| an undriven net that also feeds a stub's `input` | `undriven`, as above (it was rooted at the stub that reads it) | mununu#1180 |
| an `inout` port of a stub, or a port of a cell no module declares (no direction anywhere) | "on a port of black box `pad`": the port may drive the net and the lifted design does not say so — refused by default naming every such cell type, sorted; under the opt-in the `clock-rooted-at-black-box` note lists them (the stub was named as the driver) | mununu#1180 |
| an `inout` port of a declared (empty) black box | measured: `cutpoint -blackbox` puts a `$anyseq` on it, so it is the cut-net row above | mununu#1180 (no code change) |
| a bit with several cell outputs (two stubs' outputs; a `$and` and a stub's output) | refused by default naming every driver, sorted ("driven by more than one cell (`$and`, `osc`)"); under the opt-in the `clock-multi-driven` note (the last cell in cell order won). Two `$anyseq` cells on one bit — two declared black boxes — stay one cut net | mununu#1180 |
| a bit with a known driver, or a top-level input, that is also on a port that may drive it | the known driver (or the input) is the root; the port is not reported — a pad that reads the clock is the common shape, and refusing it would refuse every design with one. Stated in the `SOUNDNESS:` block of `root_of` and pinned by `x1180_an_input_wins_over_a_port_of_unknown_direction`: a stub whose `inout` does drive a top-level clock input lifts by default as an input clock, and no report names it | mununu#1180 (decided, not a fix) |
| `--cutpoint` on a clock net under the opt-in | lifts as a cut root with its note; the `control-slice` note's "a definite HOLDS transfers" now says it does not extend to the storage cells on a clock with no driver, naming the net and the note (a cut clock is not a freed input: the model ticks it on every step) | mununu#1180 |
| a parametrised missing module | measured: the flattened netlist keeps the cell under the module's plain name (`pll`, no `$paramod…`) and the snapshot declares it, so it is a stub like any other | mununu#1180 (no code change) |
| a snapshot that is not a netlist — a truncated file, an empty one, text that is not JSON, JSON that is not an object, no `modules` object or an empty one, a module with no `cells` object — on a fresh lift (the hierarchy snapshot or the flattened netlist) | a lift error naming the file and what is wrong with it, a mununu defect: the lift's own script wrote the file (`read_lift_snapshot` checks both with `snapshot_modules`; a missing or empty file was already refused, mununu#957). The clock scan returns `Err` for such a text and `Ok` with no domain only for a netlist with no storage cell (it returned no domains for both, so nothing was refused and nothing was noted) | mununu#1198 |
| the same damage in a lift cache entry (`MUNUNU_LIFT_CACHE_DIR`), in either snapshot | a cache miss, logged at `warn` with the snapshot and the damage; the lift runs and replaces the entry (it was a hit: a two-clock design lifted from it with no refusal by default, and with no `clock-domains-merged` note under the opt-in). A cache hit that still reaches the scan with such a netlist is a lift error, a mununu defect | mununu#1198 |
| the same damage in the hierarchy snapshot of the per-module lift's discovery pass | a lift error that names the snapshot and the damage, a mununu defect (it was an error there too, but one that named the design: "found no non-blackbox modules in the hierarchy snapshot") | mununu#1198 |
| the other readers of the two snapshots (the alias and cell-source maps of the flattened netlist; the port-direction, black-box, undefined-module and submodule readers of the hierarchy) | unchanged: each still answers "nothing found" for a text it cannot parse, and none is reached with one — every snapshot passes the check above on the way in, from a fresh lift or from the cache | mununu#1198 (no code change) |
| the reach cost of refusing a storage cell on a constant clock, and a lowering that keeps the design (the cell keeps its value) | open: not measured (mununu#1199) | — |
| which verbs carry the note | `--merge-clock-domains` is on eight `sv` verbs and every API `sv` route, and each reports what it merged: `sv verify-auto` in `notes` and `diagnostics.merged_clock_domains`, `sv cdc-lint` in `domains` and `notes`, every other verb and route (`sv verify-liveness*`, `sv verify-recoverability`, `sv check-fsm`, `sv lint`, `sv mutate`, `sv/plan`, `sv/cegar`) in `lift_notes` — the `clock-domains-merged` caveat and one note per free-source root, from the function `sv verify-auto` builds its own with. A free-source clock lifts under the opt-in on every verb. Before, those verbs dropped the domains: a derived or multi-domain lift shipped a lockstep verdict with nothing in its report, and a free-source clock was refused there (the containment of #1170, `report_clock_roots`, retired here) | mununu#1148; mununu#1181 |
| a lift entry that returns the model alone (`sv_to_btor2`, `sv_discover_state_cells`, `translate_sv`) | refuses the opt-in before any tool runs, as a mununu defect: it has no report to say so in. No surface passes the opt-in to one today | mununu#1181 |
| the default refusal (several domains, a derived clock, a free-source clock) read on an operation that does not take the flag: `sv discover`, `sv cegar` on the CLI, `POST /translate` with `sv-yosys`, `verify project`, `context import` (the three entries above) | the last sentence says first that the operation has no opt-in (a mununu limitation), then names the seven verbs on which the flag accepts the merged model. The lift is told which kind of surface asked (`MergeOptIn`); the policy tests hold the named verbs to the verbs' definitions, in both directions. Before, the sentence read as if the reader's own verb took the flag ("on `sv verify-auto`" on `main`, "on the `sv` verbs" at the first head of #1200) | mununu#1181 (the cross-review of #1200) |
| a plan with no property to plan (`sv/plan`, `sv verify-auto --plan-only`) | the plan stops before the lift, so there is no lift to report and no `lift_notes` | mununu#1181 (no code change) |
| `sv cegar` on the CLI | takes none of the shared lift arguments, so the opt-in cannot reach it: open. A multi-clock design is refused there saying the verb has no opt-in (the row above) | mununu#1190 (open) |
| the per-module lift (`sv emit-btor2-per-module`, `verify project` with `multi_module = true`) | no clock scan at all: a multi-clock, derived-clock or free-source module lifts onto one clock with nothing said: open | mununu#1191 (open) |
| the lift's other decisions on those verbs (a cut module, a cut point, a parameter override, a retried lift) | reported by `sv verify-auto` alone; `lift_notes` is where they go: open | mununu#1189 (open) |
| `sv cdc-lint` (forces the merge; lints the domains) | reports the roots: a free-source clock lints, its root named in a note ("classified as from an independent clock"); before #1170 it linted with no note | mununu#1148 (the fourth cross-review found the refusal there) |

**Enumerating test:** `x1148_a_clock_with_no_driver_is_named_in_the_lint_notes` (cdc-lint), `every_lift_that_can_merge_clock_domains_reports_them` (policy: the lift entries that drop the domains refuse the opt-in, and every caller of the one that returns them is listed with its report), `every_clock_root_kind_is_classified_and_described` (exhaustive match over `ClockRoot`, one net per kind, the `inout` port included) and `the_two_black_box_shapes_are_free_roots_not_derived_domains` (the captured shapes: refused by default, accepted under the opt-in); the note's seam: `x1148_a_free_source_clock_is_a_soundness_caveat_with_its_cell`; the real lift on both shapes: `e2e_a_black_box_clock_is_refused_unless_merged_and_its_root_is_classified` (image). For mununu#1180: `x1180_every_open_clock_net_shape_is_classified_from_the_captured_netlists` (ten captures, each with the root the lift gives it and the root with no snapshot), `x1180_the_root_does_not_follow_the_cell_order`, `x1180_a_constant_clock_domain_counts_its_cells_and_names_the_first`, `x1180_each_new_root_is_refused_in_its_own_words_unless_merged`; the notes' seams `x1180_each_new_clock_root_has_its_own_note` and `x1180_the_control_slice_claim_stops_at_a_clock_with_no_driver`; the real lift: `e2e_every_open_clock_net_shape_is_classified_by_the_real_lift` (image). For mununu#1181: `e2e_x1181_every_sv_route_reports_the_merged_clock_domains` (image; one row per route and mode on a two-clock design, and the one-clock control with no `lift_notes`), `e2e_x1181_without_the_opt_in_a_two_clock_design_is_still_refused`, `x1181_every_merged_shape_has_the_caveat_and_every_free_root_its_note` (the builder over the three merged shapes and every `ClockRoot` kind), `x1181_verify_auto_and_the_other_verbs_word_the_merge_the_same`, `x1181_every_model_only_lift_refuses_the_merge_opt_in`, `x1181_a_clock_refusal_names_the_opt_in_where_the_caller_can_take_it` (every refused shape on both kinds of surface), `e2e_x1181_a_model_only_lift_says_it_has_no_merge_opt_in` (image; the three entries and the reporting one), `the_clock_refusal_names_every_verb_that_takes_the_merge_flag` (policy: no verb with the flag is left out of the sentence), `every_pinned_report_of_a_lifting_sv_verb_has_optional_lift_notes` (policy: the nine pinned report schemas) and the CLI's `x1181_a_hand_built_report_carries_the_lift_notes_and_omits_an_empty_list` / `x1181_the_plan_text_prints_each_lift_note_with_its_level`. For mununu#1198: `x1198_the_clock_scan_tells_a_damaged_snapshot_from_a_design_with_no_storage_cell` (one netlist with a clock, one with no storage cell, and every damaged text with the damage it is reported as; the scan and the shared predicate agree on each), `x1198_the_lift_refuses_a_snapshot_that_is_not_a_netlist` (the fresh lift's door: a missing file, a good one, each damaged one), `x1198_a_cache_hit_with_a_damaged_netlist_is_not_a_lift_with_no_clock` (a two-clock netlist: refused by default and lifted with two domains under the opt-in when intact, a lift error under both when damaged) and, in `lift_cache.rs`, `x1198_an_entry_with_a_damaged_snapshot_is_a_miss_and_is_replaced` (both snapshots by every damaged text); the real lift's snapshots pass the check in `e2e_lift_cache_second_lift_is_a_hit_with_identical_artifacts` (image; the second lift is a hit only if both stored snapshots are netlists).

### `cleaned-netlist-coverage`

**What varies:** what reads a storage cell, which decides whether a copy of the design cleaned by yosys's `opt_clean` still holds it. `opt_clean` keeps the cone of the output ports. `write_btor` writes more: the cone of the output ports and of every wire whose name does not start with `$` (a named wire becomes a named node and pulls its drivers in). So a copy cleaned by a bare `opt_clean` holds less than the model, and whatever reads the copy answers for a smaller design. The lift cleans two copies: the one the clock scan reads (`clocks.json`, which `sv cdc-lint`, the source map and the counterexample slice read too) and, on the backend's "topological loop" error, the design the retry writes the model from. Since mununu#1229 one pass marks every named net `keep` before each `opt_clean` (`KEEP_NAMED_NETS` in `adapter/yosys/mod.rs`), which is the backend's rule. A keep on every storage cell was tried first and rejected: it counts a flop the model has dropped (the `--cutpoint` row below). Each row was run by hand with the yosys of `mununu-sva` (0.68+120) on 2026-11-23, on the sources of the enumerating test; the design is two flops on two clocks, and the row says what reads the second domain's storage.

| Case | Behaviour | Landed |
|---|---|---|
| an output port reads it | in the model and in the copy: two domains, refused by default | already correct (mununu#758; `e2e_two_clock_design_is_refused_at_lift`) |
| only an assertion reads it (the lift reads the design with the assertions left out, so yosys sees no reader) | in the model by its name; it was not in the copy, so the design lifted as one domain with no refusal and no `clock-domains-merged` caveat, and a `holds` was given. Now two domains | mununu#1229 |
| the design has no output port | the same | mununu#1229 |
| nothing reads it and it has a name | the same: the backend writes a named register, and a property can name it | mununu#1229 |
| `--cutpoint` on the only output port's net | the same (the cut leaves the second domain's register with no output to reach) | mununu#1229 |
| `--cutpoint` on the crossing flop's own net | the cut net takes the name and a free value, and the flop behind it, now on a net with no name, is not in the model: one domain, accepted before and after. This is the "cut the crossing" recipe, and the row that rejected a keep on every storage cell | already correct, now pinned |
| a memory read through a named net that only an assertion reads | in the model with its `next`; it was not in the copy. Now two domains (the memory's write clock) | mununu#1229 |
| a memory nothing reads | not in the model, not in the copy: one domain | already correct, now pinned |
| a register whose only name starts with `$` (an escaped identifier) | not in the model, not in the copy: one domain | already correct, now pinned |
| a latch, enabled by an input, that nothing reads | in the model as a state with no name of its own; a latch has no clock pin, so it is no cell of the scan and the design reads as one domain (`e2e_a_latch_enabled_by_the_clock_level_withholds_its_violation` is the latch's own test) | unchanged |
| the backend-loop retry (the second cleaned copy) | it ran a bare `opt_clean` before `write_btor` and dropped every register no output port reads from the MODEL: 16 of the 222 registers of OpenTitan `csrng` and 234 named nodes of its BTOR2 (by hand, the same yosys and date). A property over one of them did not resolve; the note said "the same design". Now the retry keeps the named nets first and writes the registers of the first script | mununu#1229 |
| the same designs on `sv cdc-lint` | the flop rows: the domain is listed and the crossing classified. The memory row: the domain is listed, and the crossing into the memory's write port is still not classified, because the lint's capture points are flop pins: open | mununu#1229; mununu#1230 (open) |
| the per-module lift (`sv emit-btor2-per-module`, `verify project` with `multi_module = true`) | no clock scan at all: open | mununu#1191 (open) |
| a lift cache entry stored before the change | a miss: the entry's key holds the script text, which changed. The lift runs and stores a new entry | mununu#1229 (no code change) |

**Enumerating test:** `e2e_x1229_the_scan_counts_a_domain_exactly_when_the_model_holds_its_storage` (the ten reader rows: for each, whether the model holds the storage, the number of domains, and that the default lift refuses exactly when there are two), with `e2e_x1229_the_backend_loop_retry_writes_every_register_of_the_first_script` (the retry) and `e2e_x1229_cdc_lint_sees_a_domain_whose_storage_no_output_port_reads` (the lint); all three need yosys and run in the image. Without yosys: `x1229_every_opt_clean_of_the_lift_runs_right_after_the_keep_pass` (every script the lift builds or retries: no `opt_clean` without the keep pass before it) and `x1229_the_scan_counts_a_domain_whose_storage_no_output_port_reads` (the scan on the two captured snapshots). **Open:** mununu#1191, mununu#1230.

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

### `lift-script-path`

**What varies:** the paths the lift writes into the one yosys `-p` script (and the `-l` log argument), and whether the command that reads each strips quotes (yosys's own commands do; `read_slang` takes the token verbatim, measured 2026-11-20 on mununu#1138). Since mununu#1138 yosys runs inside the staging directory, so every path the caller supplies must be absolute by the time it is written. The same question has a second reader since mununu#1176: a path that reaches a Verilator build, as the directory `make` runs in (Verilator's `verilated.mk` refuses a `$(CURDIR)` with whitespace, whatever the files are called) or as a word of the generated Makefile.

| Case | Behaviour | Landed |
|---|---|---|
| a staged source, the staging directory's `-I`, the `write_json` / `write_btor` outputs | relative to the staging directory yosys runs in; the temp dir's path never reaches the script | mununu#1138 |
| a caller include directory (`read_slang -I`, verbatim) | staged as a link inside the staging directory (`inc0`, `inc1`, …) and the script names the link, so the caller's path never reaches `read_slang` — whitespace included; an empty path is refused naming `--include-dir` (a missing directory is slang's own warning, as before). Without symbolic links (not unix) the absolute path is written and whitespace, quotes and `;` are refused as before | mununu#1133, mununu#1138 |
| a caller include directory given relative (`inc`, `.`, `rel/inc2`, and the one-component `rtl` that `--design-dir rtl/` derives from each unit's parent) | joined onto mununu's working directory before it becomes the link's target (or the `-I` token without links), since yosys runs inside the staging directory, where a relative target dangles; an unreadable working directory is a refusal naming `--include-dir` | mununu#1138 (the third cross-review found the one-component case dangling) |
| the lift-cache key's view of the include directories | the manifest (format 4) carries them in search order beside the sorted walked files, so `-I A -I B` and `-I B -I A` with a shadowed header are two keys | mununu#1138 (the third cross-review found the order lost) |
| a source location inside a header reached through an include link | reads the link's name (`inc0/defs.svh`) where it read the caller's path; only the primary is mapped back (`name_staged_primary`) — open | mununu#1138 (disclosed in the briefing) |
| a per-module output path (`write_btor`, the per-module log) | made absolute (a relative `--output-dir`, or the CLI's default: the source's parent, empty for a bare file name, resolve against mununu's working directory); quoted as one token when it carries a space; refused for a `"` or a line break | mununu#1138 |
| the dev-only oracle's `read_verilog` sources | quoted as one token | mununu#1138 |
| a top module, parameter or signal name | validated against its grammar before the script, on the single-design lift (`plan_lift`) | mununu#1133 |
| the staging root itself (`MUNUNU_LIFT_STAGING_ROOT`, `TMPDIR`) | made absolute before the per-call directory is created | mununu#1138 |
| the yosys binary (`MUNUNU_YOSYS_PATH`) and the slang plugin (`MUNUNU_YOSYS_SLANG_PLUGIN`) | a relative path with a separator (`./oss-cad-suite/bin/yosys`, `tools/slang.so`) is joined onto mununu's working directory before yosys moves into the staging directory; a bare name stays a `$PATH` / plugin-search lookup | mununu#1138 |
| mununu's own Verilator work directory (the reset simulation that seeds the initial state) | created under the staging root (`MUNUNU_LIFT_STAGING_ROOT`, else the system temp directory), no longer under `temp_dir()` directly; a root whose path has whitespace is refused before the directory is made, the seeding step is skipped and its warning names the variable | mununu#1176 |
| the HTTP replay's scratch directory (`emit_testbench` + `replay`) | the same root; under a whitespace root the row says `replay: unavailable — …` and names the variable, and the bench text still arrives | mununu#1176 |
| the CLI's `--emit-testbench DIR` | the files are written; with `--replay` the row says `replay: unavailable — …; pass --emit-testbench a directory whose full path has none` before `make` starts; without `--replay` a note on stderr says `make` will not build there | mununu#1176 |
| a source path the bench's Makefile would name (whitespace, a quote, `$`, …) | no bench: `trace.replay.not_generated` carries the reason and the CLI prints `replay testbench: not written — …` (it used to end in a log line and a report that read as "no counterexample with a path") | mununu#1176 |
| a top module name a Makefile word cannot carry (`$`) | no bench, said the same way: a mununu limitation, nothing to change in the design | mununu#1176 |
| the Verilator binary's path (`MUNUNU_VERILATOR_PATH`) in the bench's Makefile | written quoted (`"$(VERILATOR)"`) | mununu#1176 |
| the bench's clock name (`--tb-clock`, `tb_clock`; the default search only tries port names), written into `tb.cpp` as a member of the model and into `replay.gtkw` | a port name (a letter or `_`, then letters, digits, `_`) or no bench, said like the source-path row, with the surface's own remedy (`--tb-clock` / `tb_clock`) | mununu#1206 (2026-11-22) |
| the pinned reset's name (from the lift), written the same way | the same grammar or no bench: a mununu limitation, nothing to change in the design | mununu#1206 |
| an input port the trace drives, written into `tb.cpp` as a member of the model | outside the port grammar: left undriven and listed in `skipped` (and in the bench's header) with its reason | mununu#1206 |
| a register the trace names, written into `tb.cpp` inside a string literal and as an expression | port names joined by `.` with element indices at the end (`u_sub.cnt`, `mem[3]`); anything else — an escaped identifier, an index inside the path (`gen[0].q`) — is not compared and listed in `skipped` | mununu#1206 |
| the same names in `replay.gtkw` (one signal per line) and in the bench's `// not compared:` header (one comment line per entry) | a signal outside the register grammar is left out of the save file; a control character in a header entry is written as `?`; a design the bench refuses has no save file | mununu#1206 |
| a SystemVerilog source under a directory with whitespace, handed to the Verilator compile (an argument, not a Makefile word) | builds and runs (measured 2026-11-22 with Verilator 5.051: Verilator's own dependency files carry the path and `make` still builds) | mununu#1176 |
| the other temporary-directory users (the slang extraction's file, the pono and rIC3 members' model files, the isolated-engine child's directory — each an argument of a tool, none a build directory) | work under a `TMPDIR` whose path has whitespace (measured 2026-11-22 in the image: those adapters' tests with the e2e ones, `sv verify-auto` and `btor2 verify` with every member answering, with and without `MUNUNU_ISOLATE_ENGINES=1`, give the same results as under a plain one); they stay under the system temporary directory | mununu#1176 |

**Enumerating test:** none in one table; `x1138_the_lift_script_names_staged_files_relative_to_the_staging_directory`, `an_include_directory_is_staged_as_a_link_the_script_names` (the relative forms included), `x1138_swapping_two_include_directories_is_another_key`, `x1138_a_relative_per_module_output_directory_is_made_absolute_for_the_script`, `a_yosys_native_path_with_a_space_is_quoted_as_one_token`, `a_relative_staging_root_is_made_absolute` and `a_relative_tool_path_with_a_separator_is_made_absolute` pin the yosys rows but the oracle's and the open one; the image's `e2e_extra_include_dirs_resolves_fragment_without_standalone_compile` under `TMPDIR="/tmp/dir with space"` is the detector for the include-directory row. The Verilator rows have a table: `policy::verilator_build_directories_1176` (`crates/mununu-core/tests/policy/`) holds every product-code `make` spawn with the directory question it asks first, and every chooser of a build directory with the checked function it takes the directory from, and fails on a new one; `x1176_a_verilator_work_directory_is_never_made_under_a_path_with_whitespace`, `x1176_the_staging_root_variable_moves_the_verilator_work_directory`, `x1176_a_build_directory_is_refused_for_whitespace_anywhere_in_its_full_path`, `x1176_a_replay_in_a_directory_with_whitespace_is_refused_before_make_starts`, `x1176_a_design_the_makefile_cannot_name_is_refused_with_its_reason` and `x1176_the_makefile_runs_the_verilator_binary_as_one_word` pin the rows one by one; the names the bench writes have their own table, `every_name_the_bench_writes_is_held_to_its_grammar` (`adapter/verilator/replay.rs`: the clock, the reset, an input port, a register and the save file, each against the same list of names outside the grammar), with `a_clock_name_that_is_not_a_port_name_yields_no_bench` as the reproducer; and the image's `e2e_compile_verilator_takes_a_source_under_a_directory_with_whitespace` is the detector for the source-path row.

### `member-stop-reason`

**What varies:** why a reach-portfolio member returned no verdict. Each reason must reach the report under its own name, with the member's progress.

| Case | Behaviour | Landed |
|---|---|---|
| a peer decided first | in no list | — |
| the member reached its own bound | `undecided`, with depth or frame, and the wall it left unused | mununu#1022, mununu#1038 |
| the interpolation member's search reached its own deadline | `overall timeout (<n>ms)[ at suffix depth <k>]`, followed by the same run-budget remedy as a query-limit stop (one clause, `RUN_BUDGET_REMEDY`: both budgets are derived from the run budget). Before, the reason named no remedy | mununu#1186 |
| the wall or the run budget stopped it | `abstained`, with progress | mununu#1043 |
| the member refused the design | `refused` | — |
| the owned SAT members' model build stopped (the node cap, the wall, a peer's verdict, the run budget) | `abstained` with the node count, or a cancellation; both members share the one build | mununu#1142 |
| the member panicked | `refused`, "ended without a result (it panicked)", in both drivers; the owned-only collector tells a disconnect from the deadline | mununu#1143 |
| a rung of the justice / response ladder panicked | named in the row's notes | mununu#1140 |
| a member's path the concrete simulator did not confirm (owned IC3 on a design with a cell wider than 128 bits; a subprocess member's witness that did not replay) | the verdict ships, and the member is listed with the reason and with whether another member found a path too (`ReachOutcome::unconfirmed`), with or without a witness, on every report that reads a reach outcome — the second table below | mununu#1143, mununu#1175 |
| an owned-only member that panics while another is still alive | its death notice on the channel (`spawn_owned`), named in `refused`; a disconnect with no notice is the backstop | mununu#1143 |
| a solver query child (cvc5, MathSAT) stopped at its query limit — by its own exit (cvc5 `--tlimit`) or by the runner's backstop, which is MathSAT's only limit | one sentence from one function, whichever fired: `<tool> stopped at its <n> ms query limit … — a budget stop, not an interpolation gap`, with the run-budget remedy; the backstop arm adds that the child was killed and how long past the limit. Before, the backstop said `<tool> exceeded <n>ms`, and a reply larger than the pipe buffer was read only after the exit | mununu#1129, mununu#1186 |
| a member stopped by its caller's flag | the text names no cause: `cancelled by the caller` (the interpolation member's search and its solver child, the exact engine's model). The flag does not say why it went up; the portfolio settles the member from `StopCause`. Before, the interpolation member said "another portfolio member decided first" under a clock stop too | mununu#1186 |
| a solver child that exits with a failure, dies on a signal, or is stopped by a cancelled run budget | named as such: the status and stderr, the signal, the run-cancelled reason | mununu#1186 |
| a refinement query (`refine.rs`: the abstract IC3 engine, the CEGAR must-precondition step) stopped at its limit | open: the callers drop the reason and report a grammar ceiling or fall back (mununu#1188) | — |

**Enumerating test:** for why the shared stop flag went up (the first and third rows), the enum `StopCause` (`PeerDecided`, `CallerCancelled`, `Wall`, `RunBudget`) in `adapter/reach_portfolio.rs`, carried by `StopSignal`: its three matches are exhaustive (`StopCause::is_clock`, `StopCause::sweep_cause`, `StopSignal::raise`), so a new cause does not compile until each says where it goes, and `every_stop_cause_reports_a_stopped_member_where_its_kind_goes` holds one row per cause against the three functions that settle a stopped member. One reader is not on the enum: the exact member's join (`await_straggler`) still reads the run budget itself to tell a budget stop from a peer's. For the other rows there is no one table; `x1142_the_sat_members_share_one_aig_model_and_its_stop`, `x1142_a_cap_stop_abstains_both_members_in_both_drivers` and the exhaustive `BuildStop` matches in `sat_bmc_built` / `ic3_built` pin the build-stop row; `x1143_the_owned_collector_tells_a_dead_member_from_one_still_running`, `x1143_a_panicked_member_is_a_named_refusal_in_both_drivers`, `x1143_the_portfolio_lists_an_unreplayed_ic3_violation_without_a_witness` and `x1143_an_unreplayed_ic3_violation_is_listed_without_a_witness` pin the panicked and unreplayed rows. For the ways a solver query child stops, `x1186_every_way_a_solver_child_stops_is_named` holds one row per way (a reply, its own exit at the limit, the backstop, the caller's flag, a cancelled run budget, a failure exit, a signal) against the one runner both solvers use, and `x1186_a_query_limit_stop_reads_the_same_whichever_mechanism_fired` matches `LimitStop` exhaustively; `x1186_a_cancelled_search_does_not_say_why_the_flag_went_up` and `a_cancelled_session_abstains_with_the_cancelled_kind` pin the caller's-flag row. `x1186_the_search_deadline_names_the_run_budget_remedy` pins the deadline row.

**The unconfirmed-path row, by report** (mununu#1175). What varies there is which report reads a reach outcome; the reports are the call sites of the functions that return one.

| Report | Where the unconfirmed path shows | Landed |
|---|---|---|
| `btor2 verify`, portfolio and owned-only (CLI, API) | `trace_replay_failed` | mununu#1143 |
| `sv verify-auto` rows decided by a rescue (safety, response, fair-cycle response, EF reachability) | `portfolio.trace_replay_failed` on the row | mununu#1175 |
| `sv verify-auto` antecedent-vacuity probe | an item of the `antecedent-reachable` note | mununu#1175 |
| `sv verify-auto` `AG EF` trap certificate | an item of the certificate's note (the reach step; the closure step claims only its proof) | mununu#1175 |
| response `verify-liveness` (`btor2` and `sv`; one pair, under fairness, a conjunction) | a line of the row's notes, on a decided row too | mununu#1175 |
| justice `verify-liveness` / `verify-justice`, the l2s rung, serial and parallel ladder | a line of the verdict's notes; the parallel merge keeps every rung's notes, whichever answered first (a contradiction between the rungs shows the unconfirmed side) | mununu#1175 |
| `verify-recoverability` assumption discovery (the non-vacuity gate) | `notes` on the discovered assumption | mununu#1175 |
| `check-fsm` (`btor2` and `sv`), with and without `--liveness` | `notes` on the register's row | mununu#1175 |
| `sv cdc-lint` gray-step check | in the finding's detail | mununu#1175 |
| `btor2 verify --fault … --target` | `trace_replay_failed` on the fault summary | mununu#1175 |
| the k-liveness ladder (the fallback of an undecided response or justice row) | no verdict rests on it — a reachable bound only moves the ladder — but `<bound> reachable` is in the row's abstention note, and the unconfirmed path is said beside it (`at <bound>, <member>: …`), as it is beside a contradiction on a bound | mununu#1175 |
| the recoverability vacuity probe; the trap certificate's closure step | nothing to show: only the proof yields a hint, and a reachable `bad` in the closure step only rejects a candidate | — |

**Enumerating test:** `policy::unconfirmed_paths_reach_every_report_1175` derives the producers (every `pub` function whose return type names a `ReachOutcome`) and the readers (every file that calls one, with the number of calls) from the source, and holds one row per reader with the call that carries the list into its report; a new producer, a new reader or one more call in a listed reader fails it. `x1175_a_path_the_simulator_did_not_confirm_reaches_the_verify_auto_row` is the reproducer; `x1175_an_unconfirmed_path_says_whether_the_result_rests_on_it_alone` pins the one wording.

### `server-side-file-read`

**What varies:** how a request to a `[server] file_root` server can name a file the server then reads, or a program it then runs.

| Case | Behaviour | Landed |
|---|---|---|
| a path field of the request | confined to the root | mununu#935 |
| a path written inside a verify config | confined against the canonical base directory and the root | mununu#935 |
| an executable named by the request | refused | mununu#935 |
| an `#include` in a C source or a header it reaches | scanned; an absolute, `..` or macro-built target is refused | mununu#994 |
| a top module, parameter or source name handed to the lift script | validated before it reaches the script | mununu#1133 |

**Enumerating test:** `policy::server_paths_resolve_through_one_function` (`crates/mununu-core/tests/policy/`). The decision "this path lies under the root" is one function, `deployment::resolve_under`; the request's path fields, `base_dir`, the paths inside a verify config and the headers the include scan follows all reach it. The test holds four exact tables: the callers of that function, the calls that confine a request's field, every place `src/api/` and `codesign/include_scan.rs` turn text into a path or read the filesystem, and every path-like field name of the API's types. A new site fails it until it is routed through the function or classified with its reason. The executable row and the config's option keys are held by `every_option_key_the_orchestrator_reads_is_classified` (`verify/config.rs`); the lift-script row is not part of this test. **Open:** the test is a scan of that layer, not of the code it calls.

### `os-dependent-test-expectation`

**What varies:** what a test assumes about the host that only the Linux gate satisfies.

| Case | Behaviour | Landed |
|---|---|---|
| a path separator or a Unix path in an expected string | build the expected path with the same join the code uses | mununu#999, mununu#1153 |
| which portfolio members had reported when the first verdict stopped the rest | assert the verdict on every host; pin the attribution with scripted members, whose finish order the test fixes | mununu#1054, mununu#1065 |
| a wall-clock bound standing in for "did not wait for the stuck member" or "started no member" | the stub never returns (so an outcome at all is the claim), or every stub would decide (so an outcome that credits none is) | mununu#996, mununu#726, mununu#816 |
| a process state read right after an asynchronous kill | poll up to a grace that still separates a leak from a slow death | mununu#998 |

**Enumerating test:** for the portfolio rows, `adapter::reach_portfolio::tests::scripted` (the full driver) and `scripted::owned` (the owned-only driver): every member's runner is a parameter of the drivers (`Runners`), each test fixes which members have reported when the first verdict lands, and the whole outcome is compared. The detectors that race the real engines stay (`portfolio_decides_reachable_without_subprocess`, `owned_only_excludes_external_engines_on_small_counter`), and the OS matrix on `main` remains the detector for the class. The portfolio rows were checked against the four red OS-matrix runs of 2026-11-14 to 11-17 (five tests, each an assertion on which members had finished).

### `policy-scan-blind-spot`

**What varies:** the shapes of product code a repository gate must scan, and the spellings of the thing it looks for. A gate that passes on an input it was written to refuse is worse than no gate: it reports PASS.

| Case | Behaviour | Landed |
|---|---|---|
| product code after a mid-file `#[cfg(test)] mod` | the module is blanked by brace depth, line numbers kept (`tests/policy/source_scan.rs`) | mununu#1144 |
| the `use std::env;` spelling of a read or a write | matched (`env::var(`, `env::var_os(`, `env::set_var(`, `env::remove_var(`) | mununu#1144 |
| a quote inside a comment | not a string literal | mununu#1144 |
| "Yosys N.NN or later / or newer", any case | a version mention | mununu#1144 |
| an ignored test's runner | one predicate, `is_orphan`, tested on its rows | mununu#1144 |
| an `e2e_` test that skips when its tool is absent | `manual:`, naming the image that has the tool | mununu#1144 |

**Enumerating test:** `x1144_a_mid_file_test_module_is_skipped_and_the_code_after_it_kept` and each gate's extended control.

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
