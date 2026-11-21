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
| an IC3 counterexample on a design the simulator cannot carry | shipped `violated`, listed in `trace_replay_failed` with or without a witness; printed by `btor2 verify` (CLI, API) — open on `sv verify-auto` and the liveness / recoverability / fsm / cdc verbs, which drop the list (mununu#1175) | mununu#1143 |
| an owned-only member that panics while another is still alive | its death notice on the channel (`spawn_owned`), named in `refused`; a disconnect with no notice is the backstop | mununu#1143 |

**Enumerating test:** none in one table; `x1142_the_sat_members_share_one_aig_model_and_its_stop`, `x1142_a_cap_stop_abstains_both_members_in_both_drivers` and the exhaustive `BuildStop` matches in `sat_bmc_built` / `ic3_built` pin the build-stop row; `x1143_the_owned_collector_tells_a_dead_member_from_one_still_running`, `x1143_a_panicked_member_is_a_named_refusal_in_both_drivers`, `x1143_the_portfolio_lists_an_unreplayed_ic3_violation_without_a_witness` and `x1143_an_unreplayed_ic3_violation_is_listed_without_a_witness` pin the panicked and unreplayed rows.

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
