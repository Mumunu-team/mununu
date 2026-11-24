//! mununu#1191 — the per-module lift (`sv emit-btor2-per-module`, and `verify project` with the
//! source option `multi_module = true`) wrote a model of each module and never looked at its
//! clocks. A module with two clock domains, or with a clock the lift cannot model, was lifted
//! as if every register stepped together, while the flattened lift of the same source refused
//! it. The clock scan had one home, and a second way to a model went round it.
//!
//! The rule is structural, read from the lift's source (`adapter/yosys/`):
//! 1. **Scripts.** Every function that writes the model-writing command into a yosys script is
//!    listed in [`MODEL_SCRIPTS`] with the call that puts the clock snapshot into the same
//!    script, and no file outside the lift writes that command.
//! 2. **Runs.** Every function that runs yosys on a script is listed in [`RUNS`] with how many
//!    runs it makes; each reads the clock snapshot back and asks the refusal. A function that
//!    starts a process at all is one of [`STARTERS`].
//! 3. **Models.** Every function that builds the value a lift returns ([`MODEL_VALUES`]) is
//!    listed in [`MODEL_BUILDERS`] with the refusal it asks first — the cache hit, which runs
//!    nothing, included.
//!
//! A new script, run or returned model fails here until it is listed with its scan, or with the
//! reason it needs none.
//!
//! What the scan cannot see: that the snapshot a script takes is the one the scan reads, and
//! that the refusal says what the flattened lift says. The lib tests named `x1191_…` pin the
//! script text and the wording at the seam, and
//! `e2e_x1191_the_per_module_lift_refuses_what_the_flattened_lift_refuses` runs the real lift.

use std::path::{Path, PathBuf};

use crate::merged_clock_domains_reported_1181::{calls, product_code, rel, top_level_fns, under};
use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};

/// The directory that holds the lift, as path components under the repository root.
const LIFT_DIR: &[&str] = &["crates", "mununu-core", "src", "adapter", "yosys"];

/// The file of the lift that builds and runs the scripts.
const LIFT_FILE: &str = "mod.rs";

/// The source directories whose product code could write a yosys script.
const SCANNED: &[&[&str]] = &[
    &["crates", "mununu-core", "src"],
    &["crates", "mununu-cli", "src"],
];

/// The yosys command that writes the model, as a script holds it (the command and the space
/// before its argument; a message that names the command in backticks does not match).
const WRITES_A_MODEL: &str = "write_btor ";

/// How a function that holds [`WRITES_A_MODEL`] gets the clock snapshot into its script.
enum Snapshot {
    /// It makes this call, which writes the snapshot passes.
    Calls(&'static str),
    /// It builds no script of its own; the reason says where the snapshot comes from.
    Inherited(&'static str),
}

/// `(a function whose product code holds the model-writing command, how its script gets the
/// clock snapshot)`.
const MODEL_SCRIPTS: &[(&str, Snapshot)] = &[
    (
        "backend_loop_retry_script",
        Snapshot::Inherited(
            "it splices a clean-up pass into a script `build_script` wrote, before that \
             script's model-writing command; the snapshot passes are already in it",
        ),
    ),
    (
        // The flattened lift: the clock scan script, then the passes that write the model.
        "build_script",
        Snapshot::Calls("build_clock_scan_script"),
    ),
    (
        // One module of the per-module lift.
        "build_per_module_script",
        Snapshot::Calls("clock_snapshot_passes"),
    ),
];

/// The function that writes the scan-only script, and the call that makes it a scan.
const SCAN_SCRIPT: (&str, &str) = ("build_clock_scan_script", "clock_snapshot_passes");

/// The two functions that run yosys on a script.
const RUNNERS: &[&str] = &["run_lift_script", "run_yosys"];

/// The call that reads the clock snapshot of a run back.
const SCAN: &str = "scan_lifted_clocks";

/// `(a function that runs yosys on a script, how many runs it makes, the refusal it asks)`.
const RUNS: &[(&str, usize, &str)] = &[
    // The flattened script and its backend-loop retry; one scan of whichever run succeeded.
    (
        "run_sv_flatten_btor2_concrete",
        2,
        "refuse_merged_clock_domains",
    ),
    // The composed design's clock scan: a script that writes no model.
    (
        "scan_composed_design_clocks",
        1,
        "refuse_merged_clock_domains",
    ),
    // The discovery script (it lists the modules and writes no model), then one script per
    // module, each scanned before its model is read.
    ("translate_sv_per_module_impl", 2, "refuse_module_clocks"),
];

/// The functions of the lift that start a process. The first two run a script; the others run
/// none: one asks yosys whether it has the slang front end, one runs sv2v, whose output the
/// flattened script then reads.
const STARTERS: &[&str] = &[
    "run_lift_script",
    "run_sv2v",
    "run_yosys",
    "yosys_can_read_slang",
];

/// The types a lift returns its model in: the flattened lift's artifacts, and one module of
/// the per-module lift.
const MODEL_VALUES: &[&str] = &["SvFlattenArtifacts", "PerModuleOutput"];

/// `(a function that builds a [`MODEL_VALUES`] value, the refusal it asks before it does)`.
const MODEL_BUILDERS: &[(&str, &str)] = &[
    // A cache hit runs nothing: it scans the stored netlist again.
    ("artifacts_from_cached", "refuse_merged_clock_domains"),
    (
        "run_sv_flatten_btor2_concrete",
        "refuse_merged_clock_domains",
    ),
    ("translate_sv_per_module_impl", "refuse_module_clocks"),
];

/// The constant that holds the composed refusal's remedy, and the file whose option list says
/// which keys a `[[sources]].options` table takes.
const COMPOSED_REMEDY: &str = "COMPOSED_MODULE_REMEDY";
const SOURCE_OPTIONS_FILE: &[&str] = &["crates", "mununu-core", "src", "verify", "config.rs"];

/// The product code of the lift's script file.
fn lift_code(root: &Path) -> String {
    let path = under(root, LIFT_DIR).join(LIFT_FILE);
    product_code(&std::fs::read_to_string(path).expect("the lift's source"))
}

/// The names of the top-level functions of `code` whose text passes `holds`, sorted.
fn fns_where(code: &str, holds: impl Fn(&str) -> bool) -> Vec<String> {
    let mut found: Vec<String> = top_level_fns(code)
        .into_iter()
        .filter(|(_, body)| holds(body))
        .map(|(name, _)| name)
        .collect();
    found.sort();
    found
}

/// The text of the top-level function `name` of `code` (with whatever follows it up to the
/// next function).
fn body_of(code: &str, name: &str) -> String {
    top_level_fns(code)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, body)| body)
        .unwrap_or_else(|| panic!("the lift has no function `{name}` — it was renamed"))
}

/// How many values of the type `ty` the text builds: `ty {` that is not the type's
/// declaration, an `impl` block or a destructuring `let`.
fn builds(body: &str, ty: &str) -> usize {
    let needle = format!("{ty} {{");
    body.match_indices(&needle)
        .filter(|(at, _)| {
            let before = &body[..*at];
            !["struct ", "impl ", "let "]
                .iter()
                .any(|k| before.ends_with(k))
                && !before.ends_with(|c: char| c.is_alphanumeric() || c == '_')
        })
        .count()
}

/// The functions of `code` that run yosys on a script, with how many runs each makes.
fn runs(code: &str) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = top_level_fns(code)
        .into_iter()
        .map(|(name, body)| (name, RUNNERS.iter().map(|r| calls(&body, r)).sum::<usize>()))
        .filter(|(_, n)| *n > 0)
        .collect();
    found.sort();
    found
}

/// The functions of `code` that build a value a lift returns its model in.
fn model_builders(code: &str) -> Vec<String> {
    fns_where(code, |body| {
        MODEL_VALUES.iter().any(|ty| builds(body, ty) > 0)
    })
}

fn sorted(names: impl IntoIterator<Item = impl ToString>) -> Vec<String> {
    let mut v: Vec<String> = names.into_iter().map(|n| n.to_string()).collect();
    v.sort();
    v
}

#[test]
fn every_script_that_writes_a_model_takes_the_clock_snapshot() {
    let root = repo_root();
    let code = lift_code(&root);

    assert_eq!(
        fns_where(&code, |body| body.contains(WRITES_A_MODEL)),
        sorted(MODEL_SCRIPTS.iter().map(|(name, _)| name)),
        "a function of the lift writes `{}` into a yosys script and is not listed in \
         MODEL_SCRIPTS (or a listed one no longer does): list it with the call that puts the \
         clock snapshot into the same script",
        WRITES_A_MODEL.trim_end()
    );
    for (name, snapshot) in MODEL_SCRIPTS {
        match snapshot {
            Snapshot::Calls(call) => assert!(
                calls(&body_of(&code, name), call) > 0,
                "`{name}` writes a model and no longer calls `{call}`: its script takes no \
                 clock snapshot, so its model is returned with its clocks unread"
            ),
            Snapshot::Inherited(why) => assert!(
                !why.is_empty(),
                "`{name}` is listed without the reason its script needs no snapshot of its own"
            ),
        }
    }
    let (scan_script, passes) = SCAN_SCRIPT;
    assert!(
        calls(&body_of(&code, scan_script), passes) > 0,
        "`{scan_script}` no longer calls `{passes}`: the scan script takes no snapshot"
    );

    // No file outside the lift writes the command: a second home for a script is a second
    // way round the scan.
    let lift_file = under(&root, LIFT_DIR).join(LIFT_FILE);
    let elsewhere: Vec<String> = SCANNED
        .iter()
        .flat_map(|dir| rust_files_under(&under(&root, dir)))
        .filter(|path| *path != lift_file)
        .filter(|path| {
            let src = std::fs::read_to_string(path).expect("readable source");
            product_code(&src).contains(WRITES_A_MODEL)
        })
        .map(|path| rel(&path, &root))
        .collect();
    assert_eq!(
        elsewhere,
        Vec::<String>::new(),
        "a file outside the lift writes `{}` into a script: build the script in the lift, \
         where its clock snapshot is checked",
        WRITES_A_MODEL.trim_end()
    );
}

#[test]
fn every_yosys_run_of_the_lift_reads_its_clock_snapshot_back() {
    let root = repo_root();
    let code = lift_code(&root);

    let want: Vec<(String, usize)> = {
        let mut v: Vec<(String, usize)> = RUNS
            .iter()
            .map(|(name, n, _)| (name.to_string(), *n))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        runs(&code),
        want,
        "a function of the lift runs yosys on a script ({RUNNERS:?}) and is not listed in RUNS \
         with that many runs: list it, and say in its row what each run writes"
    );
    for (name, _, refusal) in RUNS {
        let body = body_of(&code, name);
        assert!(
            calls(&body, SCAN) > 0 && calls(&body, refusal) > 0,
            "`{name}` runs yosys and does not both read the clock snapshot back (`{SCAN}`) and \
             ask `{refusal}`: what it lifted is used with its clocks unread"
        );
    }

    // A process started anywhere else in the lift's directory is a run this test does not see.
    let starters: Vec<String> = rust_files_under(&under(&root, LIFT_DIR))
        .into_iter()
        .flat_map(|path: PathBuf| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            fns_where(&product_code(&src), |body| body.contains("Command::new"))
        })
        .collect();
    assert_eq!(
        sorted(starters),
        sorted(STARTERS),
        "a function of the lift starts a process and is not one of STARTERS: if it runs a \
         yosys script, run it through one of {RUNNERS:?}; otherwise list it with what it runs"
    );
}

#[test]
fn every_function_that_returns_a_lifted_model_asks_the_clock_refusal() {
    let code = lift_code(&repo_root());
    assert_eq!(
        model_builders(&code),
        sorted(MODEL_BUILDERS.iter().map(|(name, _)| name)),
        "a function of the lift builds one of {MODEL_VALUES:?} and is not listed in \
         MODEL_BUILDERS (or a listed one no longer does): a model returned from a new place \
         needs the clock refusal there too"
    );
    for (name, refusal) in MODEL_BUILDERS {
        assert!(
            calls(&body_of(&code, name), refusal) > 0,
            "`{name}` returns a lifted model and no longer asks `{refusal}`"
        );
    }
}

/// The refusal of a module of a composed design ends with a fix on the surface that composes
/// modules: a key of the project file's source options. The key it names is one the option
/// list holds (mununu#1106: a remedy the user can take).
#[test]
fn the_composed_refusal_names_a_source_option_the_project_file_takes() {
    let root = repo_root();
    let code = lift_code(&root);
    let decl = format!("const {COMPOSED_REMEDY}: &str = \"");
    let from = code
        .find(&decl)
        .unwrap_or_else(|| panic!("the lift has no `{COMPOSED_REMEDY}` — it was renamed"));
    let text = &code[from + decl.len()..];
    let text = &text[..text.find("\";").expect("the constant's closing quote")];

    // The keys the text names: a backticked `key = value`.
    let keys: Vec<&str> = text
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|span| span.split_once(" = ").map(|(key, _)| key))
        .collect();
    assert!(
        !keys.is_empty(),
        "`{COMPOSED_REMEDY}` names no source option (a backticked `key = value`): the refusal \
         of a composed module would end without the fix its surface has"
    );
    let options = std::fs::read_to_string(under(&root, SOURCE_OPTIONS_FILE)).expect("config.rs");
    let options = product_code(&options);
    for key in keys {
        assert!(
            options.contains(&format!("\"{key}\",")),
            "`{COMPOSED_REMEDY}` names the source option `{key}`, and the option lists of \
             verify/config.rs do not hold it"
        );
    }
}

/// The scans see what they are for, on sources written for the purpose: a script written
/// outside the listed functions, a run, a model built without a refusal — and not a comment, a
/// test module, a declaration or a destructuring.
#[test]
fn the_scans_see_a_new_script_a_new_run_and_a_new_model() {
    let src = r#"
/// Runs `write_btor out.btor2` — a comment, not a script.
fn listed(script: &str) -> String {
    format!("{script}; write_btor {}", "out.btor2")
}

struct SvFlattenArtifacts {
    btor2: String,
}

fn reads(a: SvFlattenArtifacts) -> String {
    let SvFlattenArtifacts { btor2 } = a;
    btor2
}

fn a_second_way(dir: &Path) -> Result<SvFlattenArtifacts, String> {
    let script = format!("read_verilog a.v; write_btor {}", dir.display());
    run_yosys(&script)?;
    not_run_yosys(&script);
    Ok(SvFlattenArtifacts { btor2: read(dir) })
}

fn run_yosys(script: &str) -> Result<(), String> {
    Command::new("yosys").arg(script).status().map(drop).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    fn in_a_test() {
        let _ = "x; write_btor y";
        run_yosys("x").unwrap();
        let _ = SvFlattenArtifacts { btor2: String::new() };
    }
}
"#;
    let code = product_code(src);
    assert_eq!(
        fns_where(&code, |body| body.contains(WRITES_A_MODEL)),
        ["a_second_way", "listed"],
        "the comment and the test module are not scripts; the two functions are"
    );
    assert_eq!(
        runs(&code),
        [("a_second_way".to_string(), 1)],
        "one run: the definition of the runner and `not_run_yosys` are not calls of it"
    );
    assert_eq!(
        model_builders(&code),
        ["a_second_way"],
        "the declaration and the destructuring build nothing"
    );
    assert_eq!(
        fns_where(&code, |body| body.contains("Command::new")),
        ["run_yosys"]
    );
    assert_eq!(calls(&body_of(&code, "a_second_way"), SCAN), 0);
}
