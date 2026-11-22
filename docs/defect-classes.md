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
| a thread with no budget poll inside one operation (an OxiDD apply) | open: stops between stages only (mununu#996) | — |
| an MCP tool call past `--request-timeout` | open: the call errors and the engine thread finishes on its own | — |

**Enumerating test:** none in one table; `x1147_a_dropped_request_cancels_its_blocking_work` and `x1147_a_cancelled_run_budget_kills_the_child_it_waits_on` pin the first and third cases.

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
| `$anyseq` from `cutpoint -blackbox` (a declared or empty black-box module: a PLL, a clock gate with no body) or from `--cutpoint` on the net | a cut net: refused by default naming the root (was refused as "derived from logic" on `main`, the `--cutpoint` case included; it was accepted only at the second head of #1170); under the opt-in it lifts with the `clock-rooted-at-black-box` note | mununu#1148 |
| a non-`$` cell with no `port_directions` (a `(* mununu_missing_module *)` stub deleted after the snapshot) | a black box: refused by default naming the cell (was a silent root — the case of mununu#1148); under the opt-in it lifts with the same note | mununu#1148 |
| a non-`$` cell with `port_directions` (a hand-written fixture; no lift path found that produces it) | a black box, as above | mununu#1148 |
| the other edge of a net already used as a clock | its own domain | mununu#758 |
| an undriven net | refused by default (`undriven`; was a silent root); under the opt-in the `clock-undriven` note | mununu#1148 |
| a constant clock bit | open: not a domain, silent | mununu#1180 |
| a net only on stub port lists; an undriven net that also feeds a stub input | open: attributed to the last stub inserted | mununu#1180 |
| an `inout` port of a black box | open: reads as undriven | mununu#1180 |
| a bit with two drivers | open: the last cell inserted wins | mununu#1180 |
| `--cutpoint` on a clock net under the opt-in | lifts as a cut root with the note beside the `control-slice` caveat, whose "a definite HOLDS transfers" does not hold for a clock: open | mununu#1180 |
| a parametrised missing module (a `$paramod…` cell type) | open: the candidate path skips `$`-typed cells, so the net reads undriven and the remedy says connect the clock — refused, wrong advice; not measured | mununu#1180 |
| which verbs carry the note | `--merge-clock-domains` is on eight `sv` verbs and every API `sv` route; a free-source clock lifts under it only on `sv verify-auto` and `sv cdc-lint`, the verbs that report the roots (`report_clock_roots`, set in `lift_sv` and `sv_cdc_lint`) — on the others the opt-in is refused naming those two verbs; a derived or multi-domain lift under the opt-in on those verbs ships no `clock-domains-merged` caveat: open | mununu#1148; mununu#1181 (open) |
| `sv cdc-lint` (forces the merge; lints the domains) | reports the roots (`report_clock_roots`): a free-source clock lints, its root named in a note ("classified as from an independent clock"); before #1170 it linted with no note | mununu#1148 (the fourth cross-review found the refusal there) |

**Enumerating test:** `x1148_a_clock_with_no_driver_is_named_in_the_lint_notes` (cdc-lint), `report_clock_roots_is_set_only_where_the_roots_are_reported` (policy: the setters), `every_clock_root_kind_is_classified_and_described` (exhaustive match over `ClockRoot`, one net per kind, the `inout` port included) and `the_two_black_box_shapes_are_free_roots_not_derived_domains` (the captured shapes: refused by default, accepted under the opt-in); the note's seam: `x1148_a_free_source_clock_is_a_soundness_caveat_with_its_cell`; the real lift on both shapes: `e2e_a_black_box_clock_is_refused_unless_merged_and_its_root_is_classified` (image).

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

**What varies:** the paths the lift writes into the one yosys `-p` script (and the `-l` log argument), and whether the command that reads each strips quotes (yosys's own commands do; `read_slang` takes the token verbatim, measured 2026-11-20 on mununu#1138). Since mununu#1138 yosys runs inside the staging directory, so every path the caller supplies must be absolute by the time it is written.

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
| the Verilator replay's harness directory (`temp_dir()`, `make -C`) | open: a temp path with a space most likely breaks the replay | mununu#1176 |

**Enumerating test:** none in one table; `x1138_the_lift_script_names_staged_files_relative_to_the_staging_directory`, `an_include_directory_is_staged_as_a_link_the_script_names` (the relative forms included), `x1138_swapping_two_include_directories_is_another_key`, `x1138_a_relative_per_module_output_directory_is_made_absolute_for_the_script`, `a_yosys_native_path_with_a_space_is_quoted_as_one_token`, `a_relative_staging_root_is_made_absolute` and `a_relative_tool_path_with_a_separator_is_made_absolute` pin every row but the oracle's and the open one; the image's `e2e_extra_include_dirs_resolves_fragment_without_standalone_compile` under `TMPDIR="/tmp/dir with space"` is the detector for the include-directory row.

### `member-stop-reason`

**What varies:** why a reach-portfolio member returned no verdict. Each reason must reach the report under its own name, with the member's progress.

| Case | Behaviour | Landed |
|---|---|---|
| a peer decided first | in no list | — |
| the member reached its own bound | `undecided`, with depth or frame, and the wall it left unused | mununu#1022, mununu#1038 |
| the wall or the run budget stopped it | `abstained`, with progress | mununu#1043 |
| the member refused the design | `refused` | — |
| the owned SAT members' model build stopped (the node cap, the wall, a peer's verdict, the run budget) | `abstained` with the node count, or a cancellation; both members share the one build | mununu#1142 |
| the member panicked | `refused`, "ended without a result (it panicked)", in both drivers; the owned-only collector tells a disconnect from the deadline | mununu#1143 |
| a rung of the justice / response ladder panicked | named in the row's notes | mununu#1140 |
| a member's path the concrete simulator did not confirm (owned IC3 on a design with a cell wider than 128 bits; a subprocess member's witness that did not replay) | the verdict ships, and the member is listed with the reason and with whether another member found a path too (`ReachOutcome::unconfirmed`), with or without a witness, on every report that reads a reach outcome — the second table below | mununu#1143, mununu#1175 |
| an owned-only member that panics while another is still alive | its death notice on the channel (`spawn_owned`), named in `refused`; a disconnect with no notice is the backstop | mununu#1143 |

**Enumerating test:** for why the shared stop flag went up (the first and third rows), the enum `StopCause` (`PeerDecided`, `CallerCancelled`, `Wall`, `RunBudget`) in `adapter/reach_portfolio.rs`, carried by `StopSignal`: its three matches are exhaustive (`StopCause::is_clock`, `StopCause::sweep_cause`, `StopSignal::raise`), so a new cause does not compile until each says where it goes, and `every_stop_cause_reports_a_stopped_member_where_its_kind_goes` holds one row per cause against the three functions that settle a stopped member. One reader is not on the enum: the exact member's join (`await_straggler`) still reads the run budget itself to tell a budget stop from a peer's. For the other rows there is no one table; `x1142_the_sat_members_share_one_aig_model_and_its_stop`, `x1142_a_cap_stop_abstains_both_members_in_both_drivers` and the exhaustive `BuildStop` matches in `sat_bmc_built` / `ic3_built` pin the build-stop row; `x1143_the_owned_collector_tells_a_dead_member_from_one_still_running`, `x1143_a_panicked_member_is_a_named_refusal_in_both_drivers`, `x1143_the_portfolio_lists_an_unreplayed_ic3_violation_without_a_witness` and `x1143_an_unreplayed_ic3_violation_is_listed_without_a_witness` pin the panicked and unreplayed rows.

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
