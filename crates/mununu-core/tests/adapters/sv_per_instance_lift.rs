//! mununu#1248 — the per-module lift gives each instance the model the design elaborates.
//!
//! The lift used to read every module type once, as a top of its own, so an instance the
//! design gives other parameter values was lifted at the type's defaults. What varies is how
//! an instance differs from the default elaboration of its type; the two tables below
//! enumerate that:
//!
//! - the lift table compares the model of each instance with the model of the same module in a
//!   design where the instantiated values are written as the defaults and nothing is
//!   overridden, and with the model at the row's defaults, which must differ;
//! - the verdict table composes the per-instance models and compares the verdicts of two
//!   properties with the flattened lift of the same design, and with the value the design has.
//!
//! Both need a yosys that reads SystemVerilog. The parts that need no tool — the grouping of
//! instances into models, the names, the script — are pinned beside the code in
//! `adapter/yosys/mod.rs` (`x1248_…`).
//!
//! engine: none decides here. The lift is yosys-slang (`read_slang --keep-hierarchy`) to one
//! BTOR2 per instance and the BTOR2 adapter; the verdicts are the verify framework's explicit
//! evaluation of the composed model and of the flattened one.

use mununu_core::adapter::AdapterOptions;
use mununu_core::adapter::yosys::multi_module::{clts_to_ctxdsl, compose_sv_multi_module};
use mununu_core::adapter::yosys::{YosysOptions, translate_sv_per_module};
use mununu_core::verify::config::VerifyConfig;
use mununu_core::verify::verify_project;
use std::collections::BTreeMap;

/// A register that follows `go`, or stays at 0 when `STUCK` is set. `default` is the default
/// of `STUCK`.
fn child(default: u8) -> String {
    format!(
        "module child #(parameter bit STUCK = {default}) \
         (input logic clk, input logic rst, input logic go, output logic busy);\n\
         \x20 always_ff @(posedge clk or posedge rst) begin\n\
         \x20   if (rst) busy <= 1'b0;\n\
         \x20   else busy <= STUCK ? 1'b0 : go;\n\
         \x20 end\n\
         endmodule\n"
    )
}

/// A top with the three inputs every child here reads, and `body` inside.
fn top(body: &str) -> String {
    format!("module top(input logic clk, input logic rst, input logic go);\n{body}endmodule\n")
}

/// One instance of `child` named `u_c`, with `overrides` between the type and the name
/// (`#(.STUCK(0))`, or nothing).
fn one_child(default: u8, overrides: &str) -> String {
    one_child_and(default, overrides, "")
}

/// [`one_child`] with `after` on the line below the instance.
fn one_child_and(default: u8, overrides: &str, after: &str) -> String {
    child(default)
        + &top(&format!(
            "  logic busy;\n  child {overrides} u_c(.clk(clk), .rst(rst), .go(go), .busy(busy));\n{after}"
        ))
}

/// Instances of `child`, one per `(name, STUCK)`, each driving `busy_<name>`.
fn children(instances: &[(&str, u8)]) -> String {
    let mut body = String::new();
    for (name, stuck) in instances {
        body += &format!(
            "  logic busy_{name};\n  child #(.STUCK({stuck})) u_{name}\
             (.clk(clk), .rst(rst), .go(go), .busy(busy_{name}));\n"
        );
    }
    child(1) + &top(&body)
}

/// The child's default comes from a package.
fn packaged(default: u8, overrides: &str) -> String {
    format!("package cfg;\n  parameter bit STUCK_DEFAULT = {default};\nendpackage\n")
        + &one_child(default, overrides).replace(
            &format!("parameter bit STUCK = {default}"),
            "parameter bit STUCK = cfg::STUCK_DEFAULT",
        )
}

/// The child's behaviour is chosen by a type parameter: it follows `go` when the type is one
/// bit wide.
fn typed(default: &str, overrides: &str) -> String {
    format!(
        "module child #(parameter type T = {default}) \
         (input logic clk, input logic rst, input logic go, output logic busy);\n\
         \x20 always_ff @(posedge clk or posedge rst) begin\n\
         \x20   if (rst) busy <= 1'b0;\n\
         \x20   else busy <= ($bits(T) == 1) ? go : 1'b0;\n\
         \x20 end\n\
         endmodule\n"
    ) + &top(&format!(
        "  logic busy;\n  child {overrides} u_c(.clk(clk), .rst(rst), .go(go), .busy(busy));\n"
    ))
}

/// A generate block picks the register's clock under a parameter: the input gated by `en`, or
/// the input itself.
fn clock_chosen(default: u8, overrides: &str) -> String {
    format!(
        "module child #(parameter bit GATE = {default}) (input logic clk, input logic rst, \
         input logic en, input logic go, output logic busy);\n\
         \x20 logic gclk;\n\
         \x20 generate\n\
         \x20   if (GATE) begin : g\n\
         \x20     assign gclk = clk & en;\n\
         \x20   end else begin : g\n\
         \x20     assign gclk = clk;\n\
         \x20   end\n\
         \x20 endgenerate\n\
         \x20 always_ff @(posedge gclk or posedge rst) begin\n\
         \x20   if (rst) busy <= 1'b0;\n\
         \x20   else busy <= go;\n\
         \x20 end\n\
         endmodule\n\
         module top(input logic clk, input logic rst, input logic go, input logic en);\n\
         \x20 logic busy;\n\
         \x20 child {overrides} u_c(.clk(clk), .rst(rst), .en(en), .go(go), .busy(busy));\n\
         endmodule\n"
    )
}

/// The child reads and drives an interface port.
fn through_an_interface(default: u8, overrides: &str) -> String {
    format!(
        "interface bus_if(input logic clk);\n\
         \x20 logic go;\n\
         \x20 logic busy;\n\
         \x20 modport sink(input clk, input go, output busy);\n\
         endinterface\n\
         module child #(parameter bit STUCK = {default}) (input logic rst, bus_if.sink b);\n\
         \x20 always_ff @(posedge b.clk or posedge rst) begin\n\
         \x20   if (rst) b.busy <= 1'b0;\n\
         \x20   else b.busy <= STUCK ? 1'b0 : b.go;\n\
         \x20 end\n\
         endmodule\n"
    ) + &top(&format!(
        "  bus_if bi(.clk(clk));\n  assign bi.go = go;\n  child {overrides} u_c(.rst(rst), .b(bi));\n"
    ))
}

/// A generate loop instantiates the child three times, at the indices -1, 0 and 1; only the
/// one at 0 follows `go`. The negative index is there for its name (`gen[-1]`).
fn generated() -> String {
    child(1)
        + &top(
            "  logic [2:0] busy;\n  for (genvar i = -1; i < 2; i++) begin : gen\n    \
             child #(.STUCK(i != 0)) u_g(.clk(clk), .rst(rst), .go(go), .busy(busy[i + 1]));\n  \
             end\n",
        )
}

/// The override is given one level up: the top sets `S` on `mid`, which hands it to its
/// `leaf`. `default` is the default of `S`.
fn nested(default: u8, overrides: &str) -> String {
    child(1).replace("module child", "module leaf")
        + &format!(
            "module mid #(parameter bit S = {default}) \
             (input logic clk, input logic rst, input logic go, output logic busy);\n\
             \x20 leaf #(.STUCK(S)) u_l(.clk(clk), .rst(rst), .go(go), .busy(busy));\n\
             endmodule\n"
        )
        + &top(&format!(
            "  logic busy;\n  mid {overrides} u_m(.clk(clk), .rst(rst), .go(go), .busy(busy));\n"
        ))
}

/// One model of a per-module lift: the instances it is the elaboration of, and its BTOR2
/// without the comment lines (yosys names the module and the file only there).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Model {
    instances: Vec<String>,
    btor: Vec<String>,
}

/// The per-module lift of `design` (top `top`), by model name. The BTOR2 is read from the
/// files the lift persists.
fn lift(design: &str) -> BTreeMap<String, Model> {
    let dir = tempfile::tempdir().expect("a directory");
    let yopts = YosysOptions {
        top: Some("top".to_string()),
        per_module_btor: true,
        per_module_output_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let outputs = translate_sv_per_module(design, &AdapterOptions::default(), &yopts)
        .unwrap_or_else(|err| panic!("the per-module lift failed: {err}\n{design}"));
    outputs
        .iter()
        .map(|out| {
            let path = out.btor2_path.as_ref().expect("the file persists");
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            let model = Model {
                instances: out.instance_paths().iter().map(|p| p.to_string()).collect(),
                btor: text
                    .lines()
                    .filter(|line| !line.starts_with(';'))
                    .map(str::to_string)
                    .collect(),
            };
            (out.module_name.clone(), model)
        })
        .collect()
}

/// What a model of a row is expected to be.
struct Expected {
    /// The model's name.
    model: &'static str,
    /// The instances it is the elaboration of.
    instances: &'static [&'static str],
    /// A design that overrides nothing and whose defaults are this instance's values, and the
    /// model to read in it: the two BTOR2 texts are the same.
    as_instantiated: (String, &'static str),
    /// The elaboration the instance does NOT have (for an overridden instance, the defaults of
    /// its type), and the model to read in it: the two texts differ.
    not_this: (String, &'static str),
}

/// An instance of `child` that follows `go` / that is stuck, against the plain two designs.
fn follows(model: &'static str, instances: &'static [&'static str]) -> Expected {
    Expected {
        model,
        instances,
        as_instantiated: (one_child(0, ""), "child"),
        not_this: (one_child(1, ""), "child"),
    }
}
fn stuck(model: &'static str, instances: &'static [&'static str]) -> Expected {
    Expected {
        model,
        instances,
        as_instantiated: (one_child(1, ""), "child"),
        not_this: (one_child(0, ""), "child"),
    }
}

/// The rows of the lift table: what differs from the default elaboration, the design, and the
/// models the lift gives.
fn lift_rows() -> Vec<(&'static str, String, Vec<Expected>)> {
    vec![
        (
            "nothing: the instance is at its type's defaults",
            one_child(1, ""),
            vec![stuck("child", &["top.u_c"])],
        ),
        (
            "a parameter override on the instance",
            one_child(1, "#(.STUCK(0))"),
            vec![follows("child", &["top.u_c"])],
        ),
        (
            "two instances of one type with the same override",
            children(&[("a", 0), ("b", 0)]),
            vec![follows("child", &["top.u_a", "top.u_b"])],
        ),
        (
            "two instances of one type with different overrides",
            children(&[("a", 0), ("b", 1)]),
            vec![
                follows("child$top.u_a", &["top.u_a"]),
                stuck("child$top.u_b", &["top.u_b"]),
            ],
        ),
        (
            "three instances of one type, two elaborations",
            children(&[("a", 0), ("b", 1), ("c", 0)]),
            vec![
                follows("child$top.u_a", &["top.u_a", "top.u_c"]),
                stuck("child$top.u_b", &["top.u_b"]),
            ],
        ),
        (
            "a defparam on the instance",
            one_child_and(1, "", "  defparam u_c.STUCK = 0;\n"),
            vec![follows("child", &["top.u_c"])],
        ),
        (
            "an override of a default the type takes from a package",
            packaged(1, "#(.STUCK(0))"),
            vec![Expected {
                model: "child",
                instances: &["top.u_c"],
                as_instantiated: (packaged(0, ""), "child"),
                not_this: (packaged(1, ""), "child"),
            }],
        ),
        (
            "a type parameter",
            typed("logic [1:0]", "#(.T(logic))"),
            vec![Expected {
                model: "child",
                instances: &["top.u_c"],
                as_instantiated: (typed("logic", ""), "child"),
                not_this: (typed("logic [1:0]", ""), "child"),
            }],
        ),
        (
            "a parameter that selects the clock in a generate block",
            clock_chosen(1, "#(.GATE(0))"),
            vec![Expected {
                model: "child",
                instances: &["top.u_c"],
                as_instantiated: (clock_chosen(0, ""), "child"),
                not_this: (clock_chosen(1, ""), "child"),
            }],
        ),
        (
            "an override on an instance with an interface port",
            through_an_interface(1, "#(.STUCK(0))"),
            vec![Expected {
                model: "child",
                instances: &["top.u_c"],
                as_instantiated: (through_an_interface(0, ""), "child"),
                not_this: (through_an_interface(1, ""), "child"),
            }],
        ),
        (
            "the index of a generate loop",
            generated(),
            vec![
                stuck(
                    "child$top.gen[-1].u_g",
                    &["top.gen[-1].u_g", "top.gen[1].u_g"],
                ),
                follows("child$top.gen[0].u_g", &["top.gen[0].u_g"]),
            ],
        ),
        (
            "an override one level up, handed down to the instance below",
            nested(1, "#(.S(0))"),
            vec![
                Expected {
                    model: "leaf",
                    instances: &["top.u_m.u_l"],
                    as_instantiated: (nested(0, ""), "leaf"),
                    not_this: (nested(1, ""), "leaf"),
                },
                Expected {
                    model: "mid",
                    instances: &["top.u_m"],
                    as_instantiated: (nested(0, ""), "mid"),
                    not_this: (nested(1, ""), "mid"),
                },
            ],
        ),
        (
            // The reader names the instance's module `child$v2$top.u_c`: the type is not the
            // text before the first `$`.
            "an override on an instance of a type whose own name has a `$`",
            one_child(1, "#(.STUCK(0))").replace("child ", "child$v2 "),
            vec![follows("child$v2", &["top.u_c"])],
        ),
    ]
}

/// The BTOR2 of `model` in the lift of `design`, lifting each design once.
fn reference(
    lifted: &mut BTreeMap<String, BTreeMap<String, Model>>,
    (design, model): &(String, &'static str),
) -> Vec<String> {
    let models = lifted.entry(design.clone()).or_insert_with(|| lift(design));
    models
        .get(*model)
        .unwrap_or_else(|| {
            panic!(
                "no model `{model}` among {:?} in\n{design}",
                models.keys().collect::<Vec<_>>()
            )
        })
        .btor
        .clone()
}

/// Every row: the lift gives exactly the models named, each for the instances named; each
/// model is the text the module has when the instance's values are its defaults, and is not
/// the text of the other elaboration.
#[test]
fn x1248_each_instance_is_lifted_as_the_design_elaborates_it() {
    if !crate::support::slang_lift_available() {
        eprintln!("skip: no yosys that reads SystemVerilog");
        return;
    }
    let mut lifted = BTreeMap::new();
    for (what, design, expected) in lift_rows() {
        let models = lift(&design);
        assert_eq!(
            models.keys().map(String::as_str).collect::<Vec<_>>(),
            {
                let mut names: Vec<&str> = expected.iter().map(|e| e.model).collect();
                names.sort_unstable();
                names
            },
            "{what}: the models of\n{design}"
        );
        for row in &expected {
            let model = &models[row.model];
            assert_eq!(
                model.instances, row.instances,
                "{what}: the instances of `{}`",
                row.model
            );
            assert_eq!(
                model.btor,
                reference(&mut lifted, &row.as_instantiated),
                "{what}: `{}` is the module with the instance's values",
                row.model
            );
            assert_ne!(
                model.btor,
                reference(&mut lifted, &row.not_this),
                "{what}: `{}` is not the other elaboration",
                row.model
            );
        }
    }
}

/// A signal of a verdict row: its name in the composition, its name in the flattened model, and
/// whether it can rise in the design.
type Signal = (&'static str, &'static str, bool);

/// The rows of the verdict table: what differs, the design, and its signals.
///
/// The clock row, the interface row and the generate-loop row are in the lift table only. The
/// register of the clock row follows `go` under either clock, so no verdict here tells the two
/// apart. The other two connect the instance through the top's own logic or through one bit of
/// a wider net, and what the composition does with those is mununu#1249.
fn verdict_rows() -> Vec<(&'static str, String, Vec<Signal>)> {
    vec![
        (
            "nothing: the instance is at its type's defaults",
            one_child(1, ""),
            vec![("u_c__busy", "busy", false)],
        ),
        (
            "a parameter override on the instance",
            one_child(1, "#(.STUCK(0))"),
            vec![("u_c__busy", "busy", true)],
        ),
        (
            "two instances of one type with different overrides",
            children(&[("a", 0), ("b", 1)]),
            vec![
                ("u_a__busy", "busy_a", true),
                ("u_b__busy", "busy_b", false),
            ],
        ),
        (
            "a defparam on the instance",
            one_child_and(1, "", "  defparam u_c.STUCK = 0;\n"),
            vec![("u_c__busy", "busy", true)],
        ),
        (
            "an override of a default the type takes from a package",
            packaged(1, "#(.STUCK(0))"),
            vec![("u_c__busy", "busy", true)],
        ),
        (
            "a type parameter",
            typed("logic [1:0]", "#(.T(logic))"),
            vec![("u_c__busy", "busy", true)],
        ),
        (
            "an override one level up, handed down to the instance below",
            nested(1, "#(.S(0))"),
            vec![("u_m__busy", "busy", true)],
        ),
    ]
}

/// A project with one source and, per signal, the two properties: the signal can rise, and
/// the signal never rises.
fn project(source: &str, signals: &[&str]) -> String {
    let mut manifest = format!(
        r#"
[project]
name = "PerInstance"

[[sources]]
id = "system"
{source}

[alphabet]
strategy = "direct"

[composition]
semantics = "synchronous"
members = ["system"]
name = "System"
"#
    );
    for (index, signal) in signals.iter().enumerate() {
        manifest += &format!(
            r#"
[[properties]]
name = "can_rise_{index}"
formula = "mu X. (({signal} == 1) || (<> X))"
over = "Circuit"

[[properties]]
name = "never_rises_{index}"
formula = "nu X. ((!({signal} == 1)) && ([] X))"
over = "Circuit"
"#
        );
    }
    manifest
}

/// Per signal, `(can rise, never rises)` as the verify framework answers on the project in
/// `dir`.
fn verdicts(manifest: &str, dir: &std::path::Path, signals: usize) -> Vec<(bool, bool)> {
    let config = VerifyConfig::from_toml(manifest).expect("the manifest parses");
    let report =
        verify_project(&config, dir).unwrap_or_else(|err| panic!("no report: {err:?}\n{manifest}"));
    let satisfied = |name: String| {
        report
            .property_verdicts
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("no verdict for `{name}`"))
            .satisfied()
    };
    (0..signals)
        .map(|index| {
            (
                satisfied(format!("can_rise_{index}")),
                satisfied(format!("never_rises_{index}")),
            )
        })
        .collect()
}

/// Every row: the composition of the per-instance models and the flattened lift of the same
/// design give the same verdicts, and they are the design's. Each signal has the two
/// properties, one of which holds in either case, so a property that could not be evaluated
/// (an atom no state carries) is a difference and not an agreement.
#[test]
fn x1248_the_composition_agrees_with_the_flattened_lift_on_every_row() {
    if !crate::support::slang_lift_available() {
        eprintln!("skip: no yosys that reads SystemVerilog");
        return;
    }
    for (what, design, signals) in verdict_rows() {
        let expected: Vec<(bool, bool)> = signals
            .iter()
            .map(|(_, _, rises)| (*rises, !*rises))
            .collect();

        let dir = tempfile::tempdir().expect("a directory");
        std::fs::write(dir.path().join("top.sv"), &design).expect("the design is written");
        let flattened_names: Vec<&str> = signals.iter().map(|(_, flat, _)| *flat).collect();
        let flattened = verdicts(
            &project(
                "adapter = \"sv-yosys\"\nfiles = [\"top.sv\"]\n\n[sources.options]\ntop = \"top\"",
                &flattened_names,
            ),
            dir.path(),
            signals.len(),
        );

        let yopts = YosysOptions {
            top: Some("top".to_string()),
            per_module_btor: true,
            ..Default::default()
        };
        let composition = compose_sv_multi_module(&design, &AdapterOptions::default(), &yopts)
            .unwrap_or_else(|err| panic!("{what}: the composition failed: {err}\n{design}"));
        let ctxdsl = clts_to_ctxdsl(&composition.composed, "Circuit", "system")
            .unwrap_or_else(|err| panic!("{what}: the composed model is not emitted: {err}"));
        std::fs::write(dir.path().join("composed.ctxdsl"), ctxdsl).expect("the model is written");
        let composed_names: Vec<&str> = signals.iter().map(|(inst, _, _)| *inst).collect();
        let composed = verdicts(
            &project(
                "adapter = \"ctxdsl\"\nfiles = [\"composed.ctxdsl\"]",
                &composed_names,
            ),
            dir.path(),
            signals.len(),
        );

        assert_eq!(
            flattened, expected,
            "{what}: the flattened lift, (can rise, never rises) per signal of\n{design}"
        );
        assert_eq!(
            composed, flattened,
            "{what}: the composition of the per-instance models against the flattened lift of\n{design}"
        );
    }
}
