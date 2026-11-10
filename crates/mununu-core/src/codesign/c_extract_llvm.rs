//! C extraction via LLVM IR — phases L1 + L2 of the principled lift.
//!
//! Implementation plan:
//! `~/.claude/plans/i-want-you-to-distributed-orbit.md`.
//!
//! ## What phase L1 ships
//!
//! - A subprocess shell-out to `clang -O0 -emit-llvm -S` (the same
//!   binary the AST extractor in [`crate::codesign::c_extract`]
//!   already invokes; no new tool dependency).
//! - The textual `.ll` output is parsed by [`crate::llvm_ir`] into a
//!   structured [`llvm_ir::Module`].
//!
//! ## What phase L2 ships
//!
//! - **Register-access identification.** Walks each function's
//!   `load volatile` / `store volatile` instructions, traces their
//!   pointer operands back through `getelementptr` /
//!   `load ptr, @global` / `inttoptr` chains, and matches the
//!   resulting symbolic address against a supplied
//!   [`RegisterMap`](crate::codesign::register_map::RegisterMap).
//! - **Linear automaton synthesis** on the same
//!   [`crate::codesign::coupling::rendezvous_label_name`] alphabet
//!   the AST extractor uses. Phase L3 will add polling-loop
//!   detection on top.
//! - Closes motivating examples 1 (macro base pointer — the IR has
//!   the literal address regardless of how the C source spells it)
//!   and 2 (`*(volatile uint32_t *)0x40010000` — the IR `inttoptr`
//!   literal is matched directly against the register map's
//!   address range).
//!
//! ## Soundness posture
//!
//! Best-effort, with unrecognised IR instructions preserved as
//! [`crate::llvm_ir::Instruction::Other`] so downstream consumers
//! can still see them. The register-access matcher emits a
//! structured [`LlvmExtractWarning::UnresolvedPointer`] when it
//! cannot fully resolve a `load volatile` / `store volatile`'s
//! pointer operand, and a [`LlvmExtractWarning::UnknownAccessor`]
//! when the resolved address is outside any register-map entry's
//! window. Nothing is silent.

use crate::codesign::c_extract::{AccessFlow, RegisterAccess};
use crate::codesign::coupling::AccessKind;
use crate::codesign::register_map::{Register, RegisterMap};
use crate::llvm_ir::{GepIndex, Instruction, Module, PointerOperand, Terminator, parse_module};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Errors raised by [`extract_c_via_llvm`].
#[derive(Debug, thiserror::Error)]
pub enum LlvmExtractError {
    /// `clang` could not be spawned (not installed, not on PATH).
    #[error(
        "could not spawn clang (tried `{tried}`): {message}. Install via xcode-select --install (macOS) or `apt install clang` (Linux), or pass --clang <path>."
    )]
    ClangNotFound { tried: String, message: String },
    /// `clang` ran but returned a non-zero exit code.
    #[error("clang exited {status}\ninvocation: {invocation}\nstderr:\n{stderr}")]
    ClangFailed {
        status: String,
        stderr: String,
        invocation: String,
    },
    /// `clang` ran cleanly but produced output the IR parser
    /// rejected.
    #[error("LLVM IR parser rejected clang output: {0}")]
    IrParseFailed(String),
    /// Failed to read the source file before invoking clang.
    #[error("failed to read source file {}: {message}", path.display())]
    SourceReadFailed { path: PathBuf, message: String },
}

/// Configuration for [`extract_c_via_llvm`]. Mirrors the AST path's
/// `CExtractOptions` shape so a CLI / API can thread options through
/// either backend transparently.
#[derive(Debug, Clone, Default)]
pub struct LlvmExtractOptions {
    /// Path to the `clang` binary. Default: `clang` (resolved via
    /// PATH).
    pub clang_path: Option<PathBuf>,
    /// Additional include paths (`-I`).
    pub include_paths: Vec<PathBuf>,
    /// Preprocessor defines (`-D`).
    pub defines: Vec<String>,
    /// Extra raw arguments to clang.
    pub extra_clang_args: Vec<String>,
    /// Phase L2: register map to match `load volatile` / `store
    /// volatile` instructions against. When `None`, the extractor
    /// returns only the structural summary (L1 behaviour).
    pub register_map: Option<RegisterMap>,
    /// Phase L2: when `true` and `register_map` is supplied, emit
    /// per-function linear CTXDSL automata on the rendezvous-label
    /// alphabet. Phase L3 will extend this with polling-loop
    /// detection.
    pub synthesize_automaton: bool,
    /// Phase L7: when `true` and ≥2 non-ISR functions have
    /// synthesised automata, emit a top-level `Driver` automaton
    /// that non-deterministically dispatches to each entry point
    /// via `call_<fn>` / `return_<fn>` rendezvous labels. Disabled
    /// by default so single-entry-point use cases get a cleaner
    /// output without the dispatch layer.
    pub driver_mode: bool,
}

/// Phase L2 warnings — the register-access matcher's structured
/// diagnostics. Mirror the AST path's `CExtractWarning` shape so a
/// downstream consumer doesn't have to fork on backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlvmExtractWarning {
    /// A `load volatile` / `store volatile` was found but its
    /// pointer operand could not be resolved through the supported
    /// chain shapes (GEP / inttoptr / load@global / bitcast). The
    /// access is dropped; the user is told.
    UnresolvedPointer { function: String, ssa: String },
    /// The pointer resolved to a symbolic address that does not lie
    /// inside any register-map entry's `[base + offset, base +
    /// offset + width]` window.
    UnknownAccessor { function: String, address: String },
    /// A bit-field load-modify-store sequence was partially
    /// recognised but slice 2 did not fully decompose it. Phase L4
    /// will handle per-field expansion.
    PartialBitfield { function: String, register: String },
    /// Phase L5: the interprocedural walker hit a recursive call
    /// (direct or transitive). The recursion is broken at the
    /// second visit; the callee's accesses are not unrolled. Sound
    /// over-approximation for safety; liveness verdicts may be
    /// affected.
    RecursiveCall { function: String },
}

impl fmt::Display for LlvmExtractWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlvmExtractWarning::UnresolvedPointer { function, ssa } => write!(
                f,
                "{function}: could not resolve pointer operand %{ssa} of a volatile load/store; access dropped"
            ),
            LlvmExtractWarning::UnknownAccessor { function, address } => write!(
                f,
                "{function}: volatile load/store at address {address} does not match any register-map entry; access dropped"
            ),
            LlvmExtractWarning::PartialBitfield { function, register } => write!(
                f,
                "{function}: bit-field load-modify-store on register {register} not yet decomposed (phase L4)"
            ),
            LlvmExtractWarning::RecursiveCall { function } => write!(
                f,
                "{function}: recursive call cycle broken \u{2014} callee accesses not unrolled (over-approximation; sound for safety, may affect liveness)"
            ),
        }
    }
}

/// Output of [`extract_c_via_llvm`]. Phase L1 emits the parsed IR as
/// a JSON record so the CLI / HTTP surface can inspect what clang
/// produced.
///
/// **Note**: the structured [`llvm_ir::Module`] is not yet
/// `Serialize` — phase L1 emits a *summary* (function names,
/// per-function basic-block + instruction counts, register-touching
/// candidates) for diagnostic purposes. Phase L2 will replace this
/// with a richer `RegisterAccess` list once the matcher lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlvmExtraction {
    /// Source filename as recorded by clang.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_filename: Option<String>,
    /// Target triple from the `.ll` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_triple: Option<String>,
    /// Per-function structural summary.
    pub functions: Vec<LlvmFunctionSummary>,
    /// Module-level external globals (typical for peripheral base
    /// pointers like `@UART`).
    pub external_globals: Vec<String>,
    /// Named struct types declared at module scope (peripheral
    /// register-bank layouts typically land here).
    pub struct_types: Vec<String>,
    /// Phase L2: structured warnings from the register-access
    /// matcher. Empty when no register map was supplied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<LlvmExtractWarning>,
    /// Phase L6: top-level CTXDSL `compositions { ... }` block
    /// emitted when at least one function carries `@mununu_isr`.
    /// Composes the main-thread automaton asynchronously with each
    /// ISR. `None` when no ISRs are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition_ctxdsl: Option<String>,
    /// Phase L7: top-level CTXDSL `Driver` automaton that non-
    /// deterministically dispatches to each non-ISR entry point.
    /// `Some` only when [`LlvmExtractOptions::driver_mode`] is set
    /// AND ≥2 non-ISR functions have synthesised automata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_ctxdsl: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlvmFunctionSummary {
    pub name: String,
    pub return_type: String,
    pub num_parameters: usize,
    pub num_basic_blocks: usize,
    pub num_instructions: usize,
    /// Number of `load volatile` / `store volatile` instructions —
    /// the candidate register accesses phase L2 will match against
    /// the register map.
    pub num_volatile_loads: usize,
    pub num_volatile_stores: usize,
    /// Number of `inttoptr` instructions — the candidate
    /// MMIO-by-literal-address accesses (the Example 2 idiom).
    pub num_inttoptr: usize,
    /// Phase L2: register accesses identified in source order. Empty
    /// when no register map was supplied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accesses: Vec<RegisterAccess>,
    /// Phase L2: CTXDSL automaton fragment synthesised from
    /// [`Self::accesses`]. `Some` only when synthesis was requested
    /// via [`LlvmExtractOptions::synthesize_automaton`] and the
    /// function has at least one matched access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automaton_ctxdsl: Option<String>,
    /// Phase L6: `true` when the function carries an `@mununu_isr`
    /// annotation. ISR functions get composed asynchronously with
    /// the main thread in [`LlvmExtraction::composition_ctxdsl`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_isr: bool,
}

/// Extract LLVM IR from a C source file and produce a structural
/// summary.
pub fn extract_c_via_llvm(
    source_path: &Path,
    options: &LlvmExtractOptions,
) -> Result<LlvmExtraction, LlvmExtractError> {
    // Touch the file to surface a friendly error before clang runs.
    if !source_path.exists() {
        return Err(LlvmExtractError::SourceReadFailed {
            path: source_path.to_path_buf(),
            message: "file not found".to_string(),
        });
    }

    let clang = options
        .clang_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("clang"));

    let mut cmd = Command::new(&clang);
    cmd.arg("-O0")
        .arg("-emit-llvm")
        .arg("-S")
        .arg("-o")
        .arg("-") // stdout
        .arg("-fno-color-diagnostics");
    for inc in &options.include_paths {
        cmd.arg("-I").arg(inc);
    }
    for define in &options.defines {
        cmd.arg(format!("-D{define}"));
    }
    for raw in &options.extra_clang_args {
        cmd.arg(raw);
    }
    cmd.arg(source_path);

    let invocation = format!("{cmd:?}");
    let output = cmd.output().map_err(|e| LlvmExtractError::ClangNotFound {
        tried: clang.display().to_string(),
        message: e.to_string(),
    })?;
    if !output.status.success() {
        return Err(LlvmExtractError::ClangFailed {
            status: format!("{}", output.status),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            invocation,
        });
    }
    let ir_text = String::from_utf8_lossy(&output.stdout).into_owned();
    // Phase L6: read the source file too, so we can lift `@mununu_*`
    // annotations from comments and tag functions accordingly.
    let source_text = std::fs::read_to_string(source_path).unwrap_or_default();
    extract_from_ir_text_with_options_and_source(&ir_text, &source_text, options)
}

/// Pure-function half of the extractor — takes an IR text and
/// produces the summary. Separated from [`extract_c_via_llvm`] so
/// tests can drive it without needing clang on `$PATH`. Uses
/// default options (no register-map matching).
pub fn extract_from_ir_text(ir_text: &str) -> Result<LlvmExtraction, LlvmExtractError> {
    extract_from_ir_text_with_options(ir_text, &LlvmExtractOptions::default())
}

/// Phase L2 entry point: parse the IR and run the register-access
/// matcher against the supplied register map.
pub fn extract_from_ir_text_with_options(
    ir_text: &str,
    options: &LlvmExtractOptions,
) -> Result<LlvmExtraction, LlvmExtractError> {
    extract_from_ir_text_with_options_and_source(ir_text, "", options)
}

/// Phase L6 entry point — same as
/// [`extract_from_ir_text_with_options`] but with the C source
/// alongside the IR so the extractor can lift `@mununu_*`
/// annotations from comments. ISR-annotated functions are tagged
/// in the output and composed asynchronously with the rest.
pub fn extract_from_ir_text_with_options_and_source(
    ir_text: &str,
    source_text: &str,
    options: &LlvmExtractOptions,
) -> Result<LlvmExtraction, LlvmExtractError> {
    let module: Module =
        parse_module(ir_text).map_err(|e| LlvmExtractError::IrParseFailed(e.to_string()))?;
    Ok(summarise(&module, source_text, options))
}

fn summarise(module: &Module, source_text: &str, options: &LlvmExtractOptions) -> LlvmExtraction {
    use crate::llvm_ir::GlobalKind;

    // Phase L6: lift `@mununu_*` annotations from C source and map
    // each `@mununu_isr` annotation to the function it sits above
    // by line-proximity scanning (the next `<ret> NAME(` declaration
    // after the annotation owns it).
    let isr_function_names: std::collections::HashSet<String> = if source_text.is_empty() {
        std::collections::HashSet::new()
    } else {
        detect_isr_functions(source_text)
    };

    let external_globals: Vec<String> = module
        .globals
        .iter()
        .filter(|g| {
            g.linkage.iter().any(|l| l == "external") || matches!(g.kind, GlobalKind::Constant)
        })
        .map(|g| g.name.clone())
        .collect();

    let struct_types: Vec<String> = module.struct_types.keys().cloned().collect();

    // Phase L5: build a module-wide function lookup so the
    // interprocedural walker can resolve `call @callee` instructions
    // to their definitions.
    let module_fns: HashMap<&str, &crate::llvm_ir::Function> = module
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();

    let mut warnings: Vec<LlvmExtractWarning> = Vec::new();
    let functions: Vec<LlvmFunctionSummary> = module
        .functions
        .iter()
        .map(|f| {
            let is_isr = isr_function_names.contains(&f.name);
            summarise_function(
                f,
                options,
                &module_fns,
                &module.string_constants,
                is_isr,
                &mut warnings,
            )
        })
        .collect();

    // Phase L6: emit a top-level CTXDSL `compositions { ... }` block
    // when any ISR functions are present. The composition is
    // asynchronous (Doc C §C.5): main-thread + each ISR interleave
    // non-deterministically.
    let composition_ctxdsl = if !isr_function_names.is_empty() {
        Some(synthesise_compositions_ctxdsl(&functions))
    } else {
        None
    };

    // Phase L7: emit a top-level Driver automaton when driver_mode
    // is on and ≥2 non-ISR entry points have synthesised automata.
    // The driver non-deterministically dispatches to each entry.
    let driver_ctxdsl = if options.driver_mode {
        let non_isr_with_automaton: Vec<&LlvmFunctionSummary> = functions
            .iter()
            .filter(|f| !f.is_isr && f.automaton_ctxdsl.is_some())
            .collect();
        if non_isr_with_automaton.len() >= 2 {
            Some(synthesise_driver_ctxdsl(&non_isr_with_automaton))
        } else {
            None
        }
    } else {
        None
    };

    LlvmExtraction {
        source_filename: module.source_filename.clone(),
        target_triple: module.target_triple.clone(),
        functions,
        external_globals,
        struct_types,
        warnings,
        composition_ctxdsl,
        driver_ctxdsl,
    }
}

fn summarise_function(
    f: &crate::llvm_ir::Function,
    options: &LlvmExtractOptions,
    module_fns: &HashMap<&str, &crate::llvm_ir::Function>,
    string_constants: &BTreeMap<String, String>,
    is_isr: bool,
    warnings: &mut Vec<LlvmExtractWarning>,
) -> LlvmFunctionSummary {
    let num_instructions: usize = f.basic_blocks.iter().map(|b| b.instructions.len()).sum();
    let num_volatile_loads = f
        .basic_blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i, Instruction::Load { volatile: true, .. }))
        .count();
    let num_volatile_stores = f
        .basic_blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i, Instruction::Store { volatile: true, .. }))
        .count();
    let num_inttoptr = f
        .basic_blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i, Instruction::IntToPtr { .. }))
        .count();

    let (accesses, automaton_ctxdsl) = if let Some(rm) = options.register_map.as_ref() {
        let mut visiting: std::collections::HashSet<String> = std::collections::HashSet::new();
        let accesses = extract_register_accesses(
            f,
            rm,
            module_fns,
            string_constants,
            &mut visiting,
            HashMap::new(), // entry-point: no caller bindings yet
            warnings,
        );
        let automaton = if options.synthesize_automaton && !accesses.is_empty() {
            Some(crate::codesign::c_extract::synthesise_automaton_ctxdsl(
                &f.name, &accesses, rm,
            ))
        } else {
            None
        };
        (accesses, automaton)
    } else {
        (Vec::new(), None)
    };

    LlvmFunctionSummary {
        name: f.name.clone(),
        return_type: f.return_type.clone(),
        num_parameters: f.parameters.len(),
        num_basic_blocks: f.basic_blocks.len(),
        num_instructions,
        num_volatile_loads,
        num_volatile_stores,
        num_inttoptr,
        accesses,
        automaton_ctxdsl,
        is_isr,
    }
}

// --------------------------------------------------------------------
// Phase L3 / L4: pre-pass plan — polling loops + bit-field RMW.
// --------------------------------------------------------------------

/// Pre-pass output used by [`extract_register_accesses`] to refine
/// what it emits.
///
/// Phase L3 + L4 both belong here because they're recognised by
/// looking at IR shapes *before* the linear walker decides what to
/// emit. Keeping the recognition in a pre-pass leaves the walker
/// itself simple.
#[derive(Debug, Default)]
struct ExtractionPlan {
    /// Phase L3: SSA names of `load volatile` instructions that are
    /// the polling read of a `while (cond) ;`-style loop. These get
    /// emitted with `flow: PollingLoop` rather than `Linear`.
    polling_loop_loads: std::collections::HashSet<String>,
    /// Phase L3: basic-block labels that are the back-edge body of a
    /// polling loop. The walker skips them entirely — they have no
    /// register accesses by construction.
    skip_blocks: std::collections::HashSet<String>,
    /// Phase L4: SSA names of `load volatile` instructions that are
    /// the read half of a bit-field read-modify-write sequence.
    /// These are silently consumed; the matching store gets
    /// resolved with [`Self::rmw_store_field`] instead of the
    /// default first-field fallback.
    rmw_consumed_loads: std::collections::HashSet<String>,
    /// Phase L4: SSA name of a `store volatile` → name of the field
    /// whose bits the RMW touches. The walker uses this to emit the
    /// right field on the store side.
    rmw_store_field: std::collections::HashMap<String, String>,
}

/// Build the pre-pass plan for a function. Scans the function once
/// and records every polling loop + every bit-field RMW pattern it
/// finds.
fn build_extraction_plan(
    f: &crate::llvm_ir::Function,
    rm: &RegisterMap,
    ctx: &ResolverCtx<'_>,
) -> ExtractionPlan {
    let ssa_defs = &ctx.ssa_defs;
    let mut plan = ExtractionPlan::default();

    // --- Phase L4 pre-pass: bit-field RMW recognition. ---
    //
    // For each `store volatile`, check whether the stored value is
    // `or (and (load volatile %ptr, MASK), BITS)` where the load
    // reads from the same pointer the store writes to. If so:
    //   - mark the load as `rmw_consumed_loads` (skip it),
    //   - compute the touched-bit mask from `(~MASK) | BITS`,
    //   - find the register-map field whose bit range overlaps the
    //     touched bits and record it under `rmw_store_field`.
    for bb in &f.basic_blocks {
        for instr in &bb.instructions {
            let Instruction::Store {
                value: crate::llvm_ir::ValueOperand::Ssa(store_value_ssa),
                dest: store_dest,
                volatile: true,
                ..
            } = instr
            else {
                continue;
            };
            // store value should trace back to `or (and load_result, BITS)`.
            let Some(or_instr) = ssa_defs.get(store_value_ssa.as_str()) else {
                continue;
            };
            let (or_a, or_b) = match or_instr {
                Instruction::BinaryOp {
                    op: crate::llvm_ir::BinaryOp::Or,
                    a,
                    b,
                    ..
                } => (a, b),
                _ => continue,
            };
            let (and_ssa, or_bits) = match (or_a, or_b) {
                (
                    crate::llvm_ir::ValueOperand::Ssa(and_ssa),
                    crate::llvm_ir::ValueOperand::LiteralInt(bits),
                )
                | (
                    crate::llvm_ir::ValueOperand::LiteralInt(bits),
                    crate::llvm_ir::ValueOperand::Ssa(and_ssa),
                ) => (and_ssa.clone(), *bits),
                _ => continue,
            };
            let Some(and_instr) = ssa_defs.get(and_ssa.as_str()) else {
                continue;
            };
            let (and_load_ssa, and_mask) = match and_instr {
                Instruction::BinaryOp {
                    op: crate::llvm_ir::BinaryOp::And,
                    a,
                    b,
                    ..
                } => match (a, b) {
                    (
                        crate::llvm_ir::ValueOperand::Ssa(load_ssa),
                        crate::llvm_ir::ValueOperand::LiteralInt(mask),
                    )
                    | (
                        crate::llvm_ir::ValueOperand::LiteralInt(mask),
                        crate::llvm_ir::ValueOperand::Ssa(load_ssa),
                    ) => (load_ssa.clone(), *mask),
                    _ => continue,
                },
                _ => continue,
            };
            // The and's first operand must be a volatile load whose
            // source is the same pointer as the store's dest.
            let Some(load_instr) = ssa_defs.get(and_load_ssa.as_str()) else {
                continue;
            };
            let Instruction::Load {
                source: load_source,
                volatile: true,
                ..
            } = load_instr
            else {
                continue;
            };
            if load_source != store_dest {
                continue;
            }
            // Compute touched bits. AND mask `M` means bits ~M are
            // cleared; OR with BITS sets those bits. So the touched
            // set is (`~M`) ∪ `BITS`. We treat as u64 for the bit-set
            // computation.
            let touched: u64 = ((!and_mask as u64) | (or_bits as u64)) & 0xFFFF_FFFF;
            // Match touched bits against register fields. We need to
            // know which register the store targets — resolve its
            // pointer through the SSA defs.
            let Some(addr) = resolve_pointer(store_dest, ctx) else {
                continue;
            };
            let register = match &addr {
                ResolvedAddress::GlobalFieldIndex { field_index, .. } => {
                    let Ok(idx) = usize::try_from(*field_index) else {
                        continue;
                    };
                    let Some(r) = rm.registers.get(idx) else {
                        continue;
                    };
                    r
                }
                ResolvedAddress::AbsoluteAddress(abs) => {
                    let Some(base) = rm.base_address_value() else {
                        continue;
                    };
                    let Some(r) = rm.registers.iter().find(|r| {
                        let lo = base.wrapping_add(r.offset);
                        let hi = lo.wrapping_add(u64::from(r.width_bits / 8));
                        *abs >= lo && *abs < hi
                    }) else {
                        continue;
                    };
                    r
                }
                ResolvedAddress::InlineConstexprGep {
                    base_addr,
                    field_index,
                } => {
                    let Some(map_base) = rm.base_address_value() else {
                        continue;
                    };
                    if *base_addr != map_base {
                        continue;
                    }
                    let Ok(idx) = usize::try_from(*field_index) else {
                        continue;
                    };
                    let Some(r) = rm.registers.get(idx) else {
                        continue;
                    };
                    r
                }
            };
            let touched_field = register.fields.iter().find(|fld| {
                // Field's bit range is [fld.bits[0] .. fld.bits[1]].
                let mut mask: u64 = 0;
                for b in fld.bits[0]..=fld.bits[1] {
                    mask |= 1u64 << b;
                }
                touched & mask != 0
            });
            let Some(touched_field) = touched_field else {
                continue;
            };
            plan.rmw_consumed_loads.insert(and_load_ssa);
            // Identify the store by its dest pointer SSA — there's
            // only one store per (load, store) pair in this pattern.
            if let PointerOperand::Ssa(dest_ssa) = store_dest {
                // We key by the store-value's SSA so the walker can
                // look it up at emit time.
                plan.rmw_store_field
                    .insert(store_value_ssa.clone(), touched_field.name.clone());
                let _ = dest_ssa; // currently unused; kept for clarity.
            }
        }
    }

    // --- Phase L3 pre-pass: polling-loop recognition. ---
    //
    // A loop header H satisfies:
    //   - H's terminator is `BrCond { cond, if_true, if_false }`.
    //   - One of {if_true, if_false} (call it B, the back-edge body)
    //     has terminator `Br { target: H }` and no volatile
    //     loads/stores in its instructions.
    //   - H contains exactly one volatile load whose result feeds
    //     `cond` (directly or via an `icmp` whose operand chains
    //     back to the load).
    for header in &f.basic_blocks {
        let Terminator::BrCond {
            cond,
            if_true,
            if_false,
        } = &header.terminator
        else {
            continue;
        };
        // Find the back-edge body — the successor that branches back
        // to the header and has no register accesses.
        let candidate_back_edges = [if_true, if_false];
        let back_edge_label = candidate_back_edges.iter().find(|&succ_label| {
            let succ = f.basic_blocks.iter().find(|b| &b.label == *succ_label);
            let Some(succ) = succ else { return false };
            if !matches!(
                succ.terminator,
                Terminator::Br { ref target } if target == &header.label
            ) {
                return false;
            }
            // The back-edge body must have no volatile loads/stores.
            !succ.instructions.iter().any(|i| {
                matches!(
                    i,
                    Instruction::Load { volatile: true, .. }
                        | Instruction::Store { volatile: true, .. }
                )
            })
        });
        let Some(&back_edge_label) = back_edge_label else {
            continue;
        };

        // Find the single volatile load in the header.
        let volatile_loads: Vec<&Instruction> = header
            .instructions
            .iter()
            .filter(|i| matches!(i, Instruction::Load { volatile: true, .. }))
            .collect();
        if volatile_loads.len() != 1 {
            continue;
        }
        let load = volatile_loads[0];
        let Instruction::Load {
            result: load_result,
            ..
        } = load
        else {
            continue;
        };
        // Trace cond back to confirm it depends on this load.
        let cond_ssa = match cond {
            crate::llvm_ir::ValueOperand::Ssa(s) => s,
            _ => continue,
        };
        if !value_depends_on(cond_ssa, load_result, ssa_defs) {
            continue;
        }

        plan.polling_loop_loads.insert(load_result.clone());
        plan.skip_blocks.insert(back_edge_label.clone());
    }

    plan
}

/// Returns `true` when the SSA value rooted at `start_ssa` transitively
/// depends on `target_ssa` via the instructions in `ssa_defs`. Used
/// to confirm a `cond` traces back to a polling-loop's volatile load.
fn value_depends_on(
    start_ssa: &str,
    target_ssa: &str,
    ssa_defs: &HashMap<&str, &Instruction>,
) -> bool {
    if start_ssa == target_ssa {
        return true;
    }
    let mut stack: Vec<&str> = vec![start_ssa];
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
    while let Some(name) = stack.pop() {
        if !visited.insert(name) {
            continue;
        }
        if name == target_ssa {
            return true;
        }
        let Some(instr) = ssa_defs.get(name) else {
            continue;
        };
        for operand in operand_ssas(instr) {
            stack.push(operand);
        }
    }
    false
}

/// Collect the SSA names an instruction's operands reference. Phase
/// L3's `value_depends_on` walks this graph backward.
fn operand_ssas<'a>(instr: &'a Instruction) -> Vec<&'a str> {
    let mut out: Vec<&'a str> = Vec::new();
    let push_pointer = |out: &mut Vec<&'a str>, p: &'a PointerOperand| {
        if let PointerOperand::Ssa(s) = p {
            out.push(s);
        }
    };
    let push_value = |out: &mut Vec<&'a str>, v: &'a crate::llvm_ir::ValueOperand| {
        if let crate::llvm_ir::ValueOperand::Ssa(s) = v {
            out.push(s);
        }
    };
    match instr {
        Instruction::Load { source, .. } => push_pointer(&mut out, source),
        Instruction::Store { value, dest, .. } => {
            push_value(&mut out, value);
            push_pointer(&mut out, dest);
        }
        Instruction::Gep { base, indices, .. } => {
            push_pointer(&mut out, base);
            for idx in indices {
                if let GepIndex::Dynamic(s) = idx {
                    out.push(s);
                }
            }
        }
        Instruction::BinaryOp { a, b, .. } | Instruction::Icmp { a, b, .. } => {
            push_value(&mut out, a);
            push_value(&mut out, b);
        }
        Instruction::Trunc { source, .. }
        | Instruction::ZExt { source, .. }
        | Instruction::SExt { source, .. }
        | Instruction::Bitcast { source, .. }
        | Instruction::PtrToInt { source, .. } => out.push(source),
        Instruction::Phi { incoming, .. } => {
            for (val, _) in incoming {
                push_value(&mut out, val);
            }
        }
        Instruction::Call { args, .. } => {
            for arg in args {
                // args are raw token strings; pick the ones that start with %.
                if let Some(rest) = arg.strip_prefix('%')
                    && !rest.is_empty()
                {
                    out.push(rest);
                }
            }
        }
        _ => {}
    }
    out
}

// --------------------------------------------------------------------
// Phase L6: ISR detection + asynchronous composition emission.
// --------------------------------------------------------------------

/// Scan C source text for `@mununu_isr` annotations and return the
/// set of function names they apply to. Each annotation owns the
/// next function declaration line below it (within a 10-line
/// window). Annotation-only — no naming-convention defaults; a
/// function not carrying `@mununu_isr` is *not* an ISR even if its
/// name ends in `_IRQHandler`.
fn detect_isr_functions(source_text: &str) -> std::collections::HashSet<String> {
    use crate::mununu_annotations::{MununuTag, extract_from_c_source};
    use regex::Regex;
    let mut out = std::collections::HashSet::new();
    let isr_annotations: Vec<u32> = extract_from_c_source(source_text)
        .into_iter()
        .filter(|a| a.tag == MununuTag::Isr)
        .filter_map(|a| a.source_line)
        .collect();
    if isr_annotations.is_empty() {
        return out;
    }
    // Scan source lines. For each ISR annotation, look for the next
    // function-declaration line within 10 lines below it.
    let fn_decl = Regex::new(r"^\s*(?:static\s+)?(?:inline\s+)?\S+\s+(\w+)\s*\(").unwrap();
    let lines: Vec<&str> = source_text.lines().collect();
    for ann_line in &isr_annotations {
        let start = *ann_line as usize;
        let end = (start + 10).min(lines.len());
        for line in lines.iter().take(end).skip(start) {
            if let Some(caps) = fn_decl.captures(line) {
                out.insert(caps[1].to_string());
                break;
            }
        }
    }
    out
}

/// Emit a top-level CTXDSL `compositions { ... }` block composing
/// the non-ISR functions asynchronously with each ISR function.
/// Doc C §C.5 mandates asynchronous composition for ISR + main-
/// thread interleaving (synchronous would be unsound — ISRs
/// preempt the main thread at arbitrary points).
fn synthesise_compositions_ctxdsl(functions: &[LlvmFunctionSummary]) -> String {
    use std::fmt::Write;
    let main_thread_fns: Vec<&LlvmFunctionSummary> = functions
        .iter()
        .filter(|f| !f.is_isr && f.automaton_ctxdsl.is_some())
        .collect();
    let isr_fns: Vec<&LlvmFunctionSummary> = functions
        .iter()
        .filter(|f| f.is_isr && f.automaton_ctxdsl.is_some())
        .collect();
    let mut buf = String::new();
    let _ = writeln!(buf, "    compositions {{");
    let _ = writeln!(buf, "        composition Codesign = asynchronous {{");
    for f in &main_thread_fns {
        let _ = writeln!(buf, "            member {};", ctxdsl_ident(&f.name));
    }
    for f in &isr_fns {
        let _ = writeln!(
            buf,
            "            member {};   // ISR",
            ctxdsl_ident(&f.name)
        );
    }
    let _ = writeln!(buf, "        }};");
    let _ = writeln!(buf, "    }}");
    buf
}

/// Phase L7: emit a top-level `Driver` automaton that non-
/// deterministically dispatches to each non-ISR entry point. Each
/// entry's automaton stays separate; the driver's role is to model
/// "the application can call any entry point at any time."
///
/// The emitted shape:
///
/// ```ctxdsl
/// automata {
///     automaton Driver {
///         controllable {
///             call_uart_send;
///             call_uart_recv;
///             return_uart_send;
///             return_uart_recv;
///         }
///         states {
///             state Idle initial;
///             state Calling_uart_send;
///             state Calling_uart_recv;
///         }
///         transitions {
///             transition Idle -> Calling_uart_send on label call_uart_send;
///             transition Calling_uart_send -> Idle on label return_uart_send;
///             transition Idle -> Calling_uart_recv on label call_uart_recv;
///             transition Calling_uart_recv -> Idle on label return_uart_recv;
///         }
///     }
/// }
/// ```
///
/// The per-function automata are unchanged; the user wires them
/// into the driver by adding matching `call_<fn>` / `return_<fn>`
/// transitions at their entry/exit states (or via a CTXDSL
/// composition block with shared labels).
fn synthesise_driver_ctxdsl(entries: &[&LlvmFunctionSummary]) -> String {
    use std::fmt::Write;
    let mut buf = String::new();
    let _ = writeln!(buf, "    automata {{");
    let _ = writeln!(buf, "        automaton Driver {{");
    let _ = writeln!(buf, "            controllable {{");
    for f in entries {
        let _ = writeln!(buf, "                call_{name};", name = f.name);
        let _ = writeln!(buf, "                return_{name};", name = f.name);
    }
    let _ = writeln!(buf, "            }}");
    let _ = writeln!(buf);
    let _ = writeln!(buf, "            states {{");
    let _ = writeln!(buf, "                state Idle initial;");
    for f in entries {
        let _ = writeln!(buf, "                state Calling_{name};", name = f.name);
    }
    let _ = writeln!(buf, "            }}");
    let _ = writeln!(buf);
    let _ = writeln!(buf, "            transitions {{");
    for f in entries {
        let _ = writeln!(
            buf,
            "                transition Idle -> Calling_{name} on label call_{name};",
            name = f.name
        );
        let _ = writeln!(
            buf,
            "                transition Calling_{name} -> Idle on label return_{name};",
            name = f.name
        );
    }
    let _ = writeln!(buf, "            }}");
    let _ = writeln!(buf, "        }}");
    let _ = writeln!(buf, "    }}");
    buf
}

/// Sanitise a function name into a CTXDSL identifier shape that
/// matches what `synthesise_automaton_ctxdsl` emits (first char
/// uppercase, rest as-is).
fn ctxdsl_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut first = true;
    for c in name.chars() {
        let safe = if c.is_ascii_alphanumeric() || c == '_' {
            c
        } else {
            '_'
        };
        if first {
            out.push(safe.to_ascii_uppercase());
            first = false;
        } else {
            out.push(safe);
        }
    }
    if out.is_empty() {
        "Func".to_string()
    } else {
        out
    }
}

// --------------------------------------------------------------------
// Phase L2: register-access identification from IR.
// --------------------------------------------------------------------

/// A resolved symbolic address — the result of tracing a pointer
/// operand back through GEP / load@global / inttoptr chains.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedAddress {
    /// `@global` was loaded directly, then GEP'd at field index `N`.
    /// E.g. `UART->REG_N` where N is the LLVM struct field index of
    /// REG within the peripheral struct.
    GlobalFieldIndex { global: String, field_index: i64 },
    /// An inline-constant byte address — `*(volatile T *)0x40010000`
    /// or an `inttoptr` instruction with a literal value.
    AbsoluteAddress(u64),
    /// An inline `getelementptr` constexpr over an `inttoptr` literal,
    /// the shape clang emits for `MACRO->FIELD` accesses where `MACRO`
    /// is `#define`d to a literal address. The matcher resolves this
    /// against the register map by checking `base_addr == map.base`
    /// and using `field_index` as the register position.
    InlineConstexprGep { base_addr: u64, field_index: i64 },
}

/// Walk `f`'s `load volatile` / `store volatile` instructions in
/// source order, resolve each pointer operand, and match the result
/// against the register map.
fn extract_register_accesses(
    f: &crate::llvm_ir::Function,
    rm: &RegisterMap,
    module_fns: &HashMap<&str, &crate::llvm_ir::Function>,
    string_constants: &BTreeMap<String, String>,
    visiting: &mut std::collections::HashSet<String>,
    param_bindings: HashMap<String, ResolvedAddress>,
    warnings: &mut Vec<LlvmExtractWarning>,
) -> Vec<RegisterAccess> {
    // Phase L5: cycle guard for the interprocedural walk. A function
    // that calls itself (directly or transitively) would otherwise
    // recurse forever.
    if !visiting.insert(f.name.clone()) {
        warnings.push(LlvmExtractWarning::RecursiveCall {
            function: f.name.clone(),
        });
        return Vec::new();
    }

    // Phase L5.5: build the resolver context — SSA defs, store-to-
    // alloca lookup, and parameter bindings from the caller.
    let ctx = build_resolver_ctx(f, param_bindings);
    let plan = build_extraction_plan(f, rm, &ctx);

    let mut accesses: Vec<RegisterAccess> = Vec::new();
    let mut current_line: u32 = 0;
    // Phase L9 (gap 1): running state-name hint, updated each time we
    // see a `call void @__mununu_state(ptr @.strN)` marker. Applied
    // to every RegisterAccess emitted until the next marker.
    let mut current_state_hint: Option<String> = None;
    for bb in &f.basic_blocks {
        if plan.skip_blocks.contains(&bb.label) {
            // Phase L3: skip the polling-loop's back-edge body. It
            // has no register accesses by construction.
            continue;
        }
        // Approximate source-line ordering by basic-block traversal.
        for instr in &bb.instructions {
            current_line = current_line.saturating_add(1);
            // Phase L5 / L5.5 / L9: dispatch on call instructions.
            // Three cases: (a) marker call to `__mununu_state` —
            // update `current_state_hint` and continue; (b) call to a
            // function defined in the same module — recurse; (c)
            // call to an external symbol — fall through silently.
            if let Instruction::Call { callee, args, .. } = instr {
                let target = callee.trim_start_matches('@');
                if target == "__mununu_state" {
                    if let Some(arg) = args.first()
                        && let Some(name) = lookup_state_marker_name(arg, string_constants)
                    {
                        current_state_hint = Some(name);
                    }
                    continue;
                }
                if let Some(callee_fn) = module_fns.get(target) {
                    let callee_bindings = build_callee_param_bindings(callee_fn, args, &ctx);
                    let callee_accesses = extract_register_accesses(
                        callee_fn,
                        rm,
                        module_fns,
                        string_constants,
                        visiting,
                        callee_bindings,
                        warnings,
                    );
                    accesses.extend(callee_accesses);
                }
                continue;
            }
            let (pointer, kind, instr_result) = match instr {
                Instruction::Load {
                    volatile: true,
                    source,
                    result,
                    ..
                } => (source, AccessKind::Read, Some(result.as_str())),
                Instruction::Store {
                    volatile: true,
                    dest,
                    value: crate::llvm_ir::ValueOperand::Ssa(value_ssa),
                    ..
                } => (dest, AccessKind::Write, Some(value_ssa.as_str())),
                Instruction::Store {
                    volatile: true,
                    dest,
                    ..
                } => (dest, AccessKind::Write, None),
                _ => continue,
            };
            // Phase L4: drop loads consumed by a bit-field RMW.
            if let (AccessKind::Read, Some(r)) = (kind, instr_result)
                && plan.rmw_consumed_loads.contains(r)
            {
                continue;
            }
            match resolve_pointer(pointer, &ctx) {
                Some(addr) => {
                    let field_override = if kind == AccessKind::Write {
                        instr_result.and_then(|r| plan.rmw_store_field.get(r).cloned())
                    } else {
                        None
                    };
                    let flow = match (kind, instr_result) {
                        (AccessKind::Read, Some(r)) if plan.polling_loop_loads.contains(r) => {
                            AccessFlow::PollingLoop
                        }
                        _ => AccessFlow::Linear,
                    };
                    if let Some(mut access) = match_address_to_register(
                        &addr,
                        rm,
                        kind,
                        current_line,
                        flow,
                        field_override.as_deref(),
                    ) {
                        access.source_state_hint = current_state_hint.clone();
                        accesses.push(access);
                    } else {
                        warnings.push(LlvmExtractWarning::UnknownAccessor {
                            function: f.name.clone(),
                            address: format!("{addr:?}"),
                        });
                    }
                }
                None => {
                    let ssa_label = match pointer {
                        PointerOperand::Ssa(s) => s.clone(),
                        PointerOperand::Global(g) => format!("@{g}"),
                        PointerOperand::InlineConstAddr(a) => format!("0x{a:x}"),
                        PointerOperand::InlineGep {
                            base_addr,
                            field_index,
                        } => format!("inline_gep(0x{base_addr:x}, field {field_index})"),
                    };
                    warnings.push(LlvmExtractWarning::UnresolvedPointer {
                        function: f.name.clone(),
                        ssa: ssa_label,
                    });
                }
            }
        }
        if let Terminator::Br { .. } | Terminator::BrCond { .. } = bb.terminator {
            current_line = current_line.saturating_add(1);
        }
    }
    visiting.remove(&f.name);
    accesses
}

/// Return the SSA-result name of an instruction, if it produces one.
fn instruction_result(instr: &Instruction) -> Option<&str> {
    Some(match instr {
        Instruction::Alloca { result, .. } => result,
        Instruction::Load { result, .. } => result,
        Instruction::Gep { result, .. } => result,
        Instruction::IntToPtr { result, .. } => result,
        Instruction::PtrToInt { result, .. } => result,
        Instruction::Bitcast { result, .. } => result,
        Instruction::Trunc { result, .. } => result,
        Instruction::ZExt { result, .. } => result,
        Instruction::SExt { result, .. } => result,
        Instruction::BinaryOp { result, .. } => result,
        Instruction::Icmp { result, .. } => result,
        Instruction::Phi { result, .. } => result,
        Instruction::Call {
            result: Some(r), ..
        } => r,
        Instruction::Call { result: None, .. } => return None,
        Instruction::Store { .. } => return None,
        Instruction::Other(_) => return None,
    })
}

/// Resolve a pointer operand to a symbolic address by walking back
/// through GEP / `load ptr, ptr @global` / `inttoptr` / `bitcast`
/// chains. Returns `None` when the chain is too complex (e.g.,
/// dynamic GEP indices, multi-step pointer arithmetic).
/// Phase L5.5: per-function context the resolver needs to handle
/// alloca-store-load round-trips and pointer-parameter aliasing.
/// Built once at the top of [`extract_register_accesses`] and
/// threaded through the resolver functions.
#[derive(Default)]
struct ResolverCtx<'a> {
    /// SSA name → producing instruction.
    ssa_defs: HashMap<&'a str, &'a Instruction>,
    /// Alloca SSA name → the single store that initialises it.
    /// Multi-store allocas (loop variables, etc.) are not tracked —
    /// the resolver returns None for those, surfacing
    /// `UnresolvedPointer`.
    store_to_alloca: HashMap<String, &'a Instruction>,
    /// Phase L5.5: function-parameter SSA name → caller's resolved
    /// argument address. Empty for the entry-point caller; populated
    /// when recursing into a callee.
    param_bindings: HashMap<String, ResolvedAddress>,
}

/// Phase L5.5: zip a call site's arguments against the callee's
/// parameters and resolve each pointer-shaped argument to a
/// `ResolvedAddress` (in the caller's context). The bindings the
/// callee will then use to resolve its own parameter SSA names.
///
/// Non-pointer arguments (i8 / i32 literals, etc.) don't matter for
/// register-access matching and are silently skipped. Pointer
/// arguments that we can't resolve in the caller (complex
/// expressions) are also skipped — the callee will surface them as
/// `UnresolvedPointer` warnings.
fn build_callee_param_bindings(
    callee: &crate::llvm_ir::Function,
    call_args: &[String],
    caller_ctx: &ResolverCtx<'_>,
) -> HashMap<String, ResolvedAddress> {
    let mut bindings = HashMap::new();
    for (param, arg_text) in callee.parameters.iter().zip(call_args.iter()) {
        let Some(param_name) = param.name.as_ref() else {
            continue;
        };
        if !param.ty.contains("ptr") {
            continue;
        }
        let pointer = parse_call_arg_as_pointer(arg_text);
        if let Some(addr) = resolve_pointer(&pointer, caller_ctx) {
            bindings.insert(param_name.clone(), addr);
        }
    }
    bindings
}

/// Cheap shape parser for a call-site argument. Strips the `<type>
/// [attrs...]` prefix and inspects the value portion. Recognises:
/// - `@global` → Global
/// - `%ssa` → Ssa
/// - `inttoptr (i64 N to ptr)` → InlineConstAddr
/// - `getelementptr (...inttoptr (i64 N to ptr)... i32 0, i32 K)` → InlineGep
fn parse_call_arg_as_pointer(arg_text: &str) -> PointerOperand {
    let trimmed = arg_text.trim().trim_end_matches(',');
    // Strip the type+attribute prefix to leave just the value
    // portion. The shapes we care about start with `@`, `%`, or a
    // keyword like `inttoptr` / `getelementptr`. Find the first such
    // token.
    let mut value_start = 0;
    for (i, c) in trimmed.char_indices() {
        if c == '@' || c == '%' {
            value_start = i;
            break;
        }
        let rest = &trimmed[i..];
        if rest.starts_with("inttoptr") || rest.starts_with("getelementptr") {
            value_start = i;
            break;
        }
    }
    let value = trimmed[value_start..].trim();
    if let Some(rest) = value.strip_prefix('@') {
        return PointerOperand::Global(rest.to_string());
    }
    if let Some(rest) = value.strip_prefix('%') {
        return PointerOperand::Ssa(rest.to_string());
    }
    // Inline `inttoptr (...)` or `getelementptr (...)` — defer to
    // the parser's shared helpers (re-exported here as
    // `parse_inline_pointer_expr`).
    if let Some(op) = crate::llvm_ir::parser::parse_inline_pointer_expr(value) {
        return op;
    }
    PointerOperand::Ssa(value.to_string())
}

fn build_resolver_ctx<'a>(
    f: &'a crate::llvm_ir::Function,
    param_bindings: HashMap<String, ResolvedAddress>,
) -> ResolverCtx<'a> {
    let mut ssa_defs: HashMap<&str, &Instruction> = HashMap::new();
    let mut store_to_alloca: HashMap<String, &Instruction> = HashMap::new();
    for bb in &f.basic_blocks {
        for instr in &bb.instructions {
            if let Some(result) = instruction_result(instr) {
                ssa_defs.insert(result, instr);
            }
            // Phase L5.5: track stores that initialise allocas.
            // Allocas have form `%X = alloca <ty>` and are
            // initialised by `store value, ptr %X`. We register only
            // the FIRST store per alloca — subsequent stores are
            // ignored (loop-bodies, conditional updates).
            if let Instruction::Store {
                dest: PointerOperand::Ssa(dest_name),
                ..
            } = instr
            {
                store_to_alloca.entry(dest_name.clone()).or_insert(instr);
            }
        }
    }
    ResolverCtx {
        ssa_defs,
        store_to_alloca,
        param_bindings,
    }
}

fn resolve_pointer(pointer: &PointerOperand, ctx: &ResolverCtx<'_>) -> Option<ResolvedAddress> {
    match pointer {
        PointerOperand::Global(name) => Some(ResolvedAddress::GlobalFieldIndex {
            global: name.clone(),
            field_index: 0,
        }),
        PointerOperand::InlineConstAddr(addr) => Some(ResolvedAddress::AbsoluteAddress(*addr)),
        PointerOperand::InlineGep {
            base_addr,
            field_index,
        } => Some(ResolvedAddress::InlineConstexprGep {
            base_addr: *base_addr,
            field_index: *field_index,
        }),
        PointerOperand::Ssa(name) => {
            // Phase L5.5: SSA names that are function parameters are
            // not in `ssa_defs` (they have no producing instruction).
            // Check the parameter bindings first.
            if let Some(addr) = ctx.param_bindings.get(name) {
                return Some(addr.clone());
            }
            let def = ctx.ssa_defs.get(name.as_str())?;
            resolve_instruction_as_address(def, ctx)
        }
    }
}

fn resolve_instruction_as_address(
    instr: &Instruction,
    ctx: &ResolverCtx<'_>,
) -> Option<ResolvedAddress> {
    match instr {
        // GEP: trace the base and add the field index. Phase L2 only
        // handles the canonical `GEP %struct.T, ptr %base, i32 0,
        // i32 K` shape — index 0 (no offset), then the field index
        // K. Anything more complex returns None (drops the access
        // with an UnresolvedPointer warning).
        Instruction::Gep { base, indices, .. } => {
            // The first index must be 0 (struct-base indirection);
            // the second index is the field-position.
            let mut field_index: Option<i64> = None;
            for (i, idx) in indices.iter().enumerate() {
                match idx {
                    GepIndex::Const(n) if i == 0 && *n != 0 => return None,
                    GepIndex::Const(n) if i == 1 => {
                        field_index = Some(*n);
                    }
                    GepIndex::Const(_) if i > 1 => {
                        // Nested GEP — phase L4 will handle bit-field
                        // sub-indexing. For L2, drop the access.
                        return None;
                    }
                    GepIndex::Dynamic(_) => return None,
                    _ => {}
                }
            }
            let field_index = field_index?;
            let base_addr = resolve_pointer(base, ctx)?;
            match base_addr {
                ResolvedAddress::GlobalFieldIndex { global, .. } => {
                    Some(ResolvedAddress::GlobalFieldIndex {
                        global,
                        field_index,
                    })
                }
                ResolvedAddress::AbsoluteAddress(addr) => {
                    // GEP through a literal address — synthesise an
                    // InlineConstexprGep with the base + new field
                    // index. Phase L4 may refine this with proper
                    // byte-offset reasoning.
                    Some(ResolvedAddress::InlineConstexprGep {
                        base_addr: addr,
                        field_index,
                    })
                }
                ResolvedAddress::InlineConstexprGep { base_addr, .. } => {
                    Some(ResolvedAddress::InlineConstexprGep {
                        base_addr,
                        field_index,
                    })
                }
            }
        }
        // `load ptr, ptr @global`: the load *result* is the
        // peripheral base pointer value. Treat it as a synonym for
        // `@global` in subsequent GEPs.
        Instruction::Load {
            source: PointerOperand::Global(name),
            volatile: false,
            ..
        } => Some(ResolvedAddress::GlobalFieldIndex {
            global: name.clone(),
            field_index: 0,
        }),
        // Phase L5.5: `load ptr, ptr %alloca` — the standard
        // alloca-store-load round-trip clang emits at -O0 to spill
        // function parameters to the stack. If %alloca has exactly
        // one store, the load's result is the stored value.
        // Resolve the stored value back through the resolver.
        Instruction::Load {
            source: PointerOperand::Ssa(alloca_name),
            volatile: false,
            ..
        } => {
            // Is %alloca_name an alloca?
            let alloca_def = ctx.ssa_defs.get(alloca_name.as_str())?;
            if !matches!(alloca_def, Instruction::Alloca { .. }) {
                return None;
            }
            // Find the store that initialised it.
            let store = ctx.store_to_alloca.get(alloca_name)?;
            let Instruction::Store { value, .. } = store else {
                return None;
            };
            // Resolve the stored value. It's typically an SSA name —
            // either an earlier instruction or a function parameter.
            match value {
                crate::llvm_ir::ValueOperand::Ssa(stored_ssa) => {
                    // Check parameter bindings first (the parameter
                    // SSA wouldn't be in ssa_defs).
                    if let Some(addr) = ctx.param_bindings.get(stored_ssa) {
                        return Some(addr.clone());
                    }
                    // Otherwise resolve the SSA via its producing
                    // instruction.
                    let def = ctx.ssa_defs.get(stored_ssa.as_str())?;
                    resolve_instruction_as_address(def, ctx)
                }
                _ => None,
            }
        }
        Instruction::IntToPtr { value, .. } => Some(ResolvedAddress::AbsoluteAddress(*value)),
        Instruction::Bitcast { source, .. } => {
            // Bitcast preserves address; resolve the source SSA.
            let def = ctx.ssa_defs.get(source.as_str())?;
            resolve_instruction_as_address(def, ctx)
        }
        _ => None,
    }
}

/// Map a resolved address back to a register-map entry.
///
/// For [`ResolvedAddress::GlobalFieldIndex`], the field index maps
/// 1:1 to a register-by-position in the register map. This is sound
/// for the common case where the C struct's field order matches the
/// register map's declaration order — exactly the codesign_uart
/// pattern. Phase L4 will refine this with byte-offset matching
/// using LLVM data layout when alignment padding diverges.
///
/// For [`ResolvedAddress::AbsoluteAddress`], the literal address is
/// matched against `[base + offset, base + offset + width]` of each
/// register-map entry.
///
/// L2 emits read/write accesses; field selection within a register
/// (which bit of a packed register is touched) is deferred to phase
/// L4. For now, an access to register `R` with `R` having one or
/// more fields produces one access per *field* if the read/write
/// touches the whole register; this is conservative
/// (over-approximation: assumes all fields were touched).
fn match_address_to_register(
    addr: &ResolvedAddress,
    rm: &RegisterMap,
    kind: AccessKind,
    source_line: u32,
    flow: AccessFlow,
    field_override: Option<&str>,
) -> Option<RegisterAccess> {
    let register: &Register = match addr {
        ResolvedAddress::GlobalFieldIndex { field_index, .. } => {
            let idx = usize::try_from(*field_index).ok()?;
            rm.registers.get(idx)?
        }
        ResolvedAddress::AbsoluteAddress(abs) => {
            let base = rm.base_address_value()?;
            rm.registers.iter().find(|r| {
                let reg_byte_offset = r.offset;
                let reg_width_bytes = u64::from(r.width_bits / 8);
                let lo = base.wrapping_add(reg_byte_offset);
                let hi = lo.wrapping_add(reg_width_bytes);
                *abs >= lo && *abs < hi
            })?
        }
        ResolvedAddress::InlineConstexprGep {
            base_addr,
            field_index,
        } => {
            // Match the constexpr base against the register-map
            // peripheral base. Mismatch → no register; the caller
            // surfaces an UnknownAccessor warning.
            let map_base = rm.base_address_value()?;
            if *base_addr != map_base {
                return None;
            }
            let idx = usize::try_from(*field_index).ok()?;
            rm.registers.get(idx)?
        }
    };

    // Phase L4: when the bit-field RMW pre-pass identified which
    // field a write touched, use that name. Otherwise fall back to
    // the lowest-bit field (phase-L2 behaviour). Whole-register
    // accesses with no fields emit `field: None`.
    let field_name = field_override.map(str::to_string).or_else(|| {
        register
            .fields
            .iter()
            .min_by_key(|f| f.bits[0])
            .map(|f| f.name.clone())
    });

    Some(RegisterAccess {
        kind,
        register: register.name.clone(),
        field: field_name,
        accessor: format!("(IR-resolved @{addr:?})"),
        source_line,
        flow,
        source_state_hint: None,
    })
}

/// Phase L9 (gap 1): parse a single argument string from a
/// `call void @__mununu_state(...)` instruction and resolve the
/// referenced string-constant global to its decoded payload.
///
/// clang lowers `MUNUNU_STATE("Polling")` to one of two shapes
/// (depending on optimisation level and target):
///
/// ```text
/// call void @__mununu_state(ptr noundef @.str)
/// call void @__mununu_state(ptr noundef getelementptr inbounds (...))
/// ```
///
/// Both forms reference a module-level `@.strN` global whose payload
/// the parser captured in `string_constants`. We accept the first
/// form natively and the second form by finding `@.strN` anywhere in
/// the argument text — the GEP indices are always `i32 0, i32 0` for
/// `[N x i8]` strings, so the global name is the only payload-bearing
/// component.
fn lookup_state_marker_name(
    arg_text: &str,
    string_constants: &BTreeMap<String, String>,
) -> Option<String> {
    let at_pos = arg_text.find('@')?;
    let after_at = &arg_text[at_pos + 1..];
    let end = after_at
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(after_at.len());
    let global_name = &after_at[..end];
    if global_name.is_empty() {
        return None;
    }
    string_constants.get(global_name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRMWARE_IR: &str = r#"; ModuleID = 'firmware.c'
source_filename = "firmware.c"
target triple = "x86_64-apple-macosx26.0.0"

%struct.UART_TypeDef = type { %union.anon, %union.anon.0, %union.anon.2 }

@UART = external constant ptr, align 8

define void @uart_send(i8 noundef zeroext %0) {
  %2 = alloca i8, align 1
  store i8 %0, ptr %2, align 1
  br label %3

3:
  %4 = load ptr, ptr @UART, align 8
  %5 = getelementptr inbounds %struct.UART_TypeDef, ptr %4, i32 0, i32 1
  %6 = load volatile i32, ptr %5, align 4
  %7 = and i32 %6, 1
  %8 = icmp ne i32 %7, 0
  br i1 %8, label %9, label %10

9:
  br label %3

10:
  %11 = load i8, ptr %2, align 1
  %12 = load ptr, ptr @UART, align 8
  %13 = getelementptr inbounds %struct.UART_TypeDef, ptr %12, i32 0, i32 2
  store volatile i8 %11, ptr %13, align 4
  %14 = load ptr, ptr @UART, align 8
  %15 = getelementptr inbounds %struct.UART_TypeDef, ptr %14, i32 0, i32 0
  %16 = load volatile i32, ptr %15, align 4
  %17 = and i32 %16, -2
  %18 = or i32 %17, 1
  store volatile i32 %18, ptr %15, align 4
  ret void
}
"#;

    #[test]
    fn summarises_uart_send_function() {
        let ext = extract_from_ir_text(FIRMWARE_IR).unwrap();
        assert_eq!(ext.functions.len(), 1);
        let f = &ext.functions[0];
        assert_eq!(f.name, "uart_send");
        assert_eq!(f.return_type, "void");
        assert_eq!(f.num_parameters, 1);
        // entry + %3 + %9 + %10
        assert_eq!(f.num_basic_blocks, 4);
        // 1 STATUS poll-load + 1 CTRL bit-field RMW load.
        assert_eq!(f.num_volatile_loads, 2);
        // 1 DATA write + 1 CTRL bit-field RMW store.
        assert_eq!(f.num_volatile_stores, 2);
        assert_eq!(f.num_inttoptr, 0);
    }

    #[test]
    fn surfaces_external_globals_and_struct_types() {
        let ext = extract_from_ir_text(FIRMWARE_IR).unwrap();
        assert!(ext.external_globals.contains(&"UART".to_string()));
        assert!(ext.struct_types.iter().any(|s| s == "%struct.UART_TypeDef"));
    }

    #[test]
    fn summary_round_trips_through_serde() {
        let ext = extract_from_ir_text(FIRMWARE_IR).unwrap();
        let json = serde_json::to_string_pretty(&ext).unwrap();
        let parsed: LlvmExtraction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.functions.len(), 1);
        assert_eq!(parsed.functions[0].name, "uart_send");
    }

    #[test]
    fn empty_ir_input_errors() {
        assert!(matches!(
            extract_from_ir_text("").unwrap_err(),
            LlvmExtractError::IrParseFailed(_)
        ));
    }

    #[test]
    fn counts_inttoptr_register_access_candidates() {
        let ir = r#"define void @enable_pll() {
  %1 = inttoptr i64 1073887232 to ptr
  %2 = load volatile i32, ptr %1, align 4
  %3 = or i32 %2, 16777216
  store volatile i32 %3, ptr %1, align 4
  ret void
}
"#;
        let ext = extract_from_ir_text(ir).unwrap();
        assert_eq!(ext.functions[0].num_inttoptr, 1);
        assert_eq!(ext.functions[0].num_volatile_loads, 1);
        assert_eq!(ext.functions[0].num_volatile_stores, 1);
    }

    // ----------------------------------------------------------------
    // Phase L2 tests — register-access matching from IR.
    // ----------------------------------------------------------------

    use crate::codesign::register_map::{
        AccessPath, Field, Register, RegisterDirection, RegisterMap, VisibilityClass,
    };

    fn uart_register_map() -> RegisterMap {
        RegisterMap {
            peripheral: "UART_LITE".to_string(),
            base_address: "0x40010000".to_string(),
            description: None,
            contract_uri: None,
            registers: vec![
                Register {
                    name: "CTRL".to_string(),
                    offset: 0,
                    width_bits: 32,
                    direction: RegisterDirection::Rw,
                    visibility_class: VisibilityClass::Control,
                    access_path: AccessPath::MmioDirect,
                    description: None,
                    fields: vec![Field {
                        name: "tx_start".to_string(),
                        bits: [0, 0],
                        sv_signal: None,
                        c_accessor: Some("UART->CTRL.bit.tx_start".to_string()),
                        description: None,
                    }],
                },
                Register {
                    name: "STATUS".to_string(),
                    offset: 4,
                    width_bits: 32,
                    direction: RegisterDirection::Ro,
                    visibility_class: VisibilityClass::Status,
                    access_path: AccessPath::MmioDirect,
                    description: None,
                    fields: vec![Field {
                        name: "tx_busy".to_string(),
                        bits: [0, 0],
                        sv_signal: None,
                        c_accessor: Some("UART->STATUS.bit.tx_busy".to_string()),
                        description: None,
                    }],
                },
                Register {
                    name: "DATA".to_string(),
                    offset: 8,
                    width_bits: 32,
                    direction: RegisterDirection::Rw,
                    visibility_class: VisibilityClass::Data,
                    access_path: AccessPath::MmioDirect,
                    description: None,
                    fields: vec![Field {
                        name: "byte".to_string(),
                        bits: [0, 7],
                        sv_signal: None,
                        c_accessor: Some("UART->DATA.byte".to_string()),
                        description: None,
                    }],
                },
            ],
        }
    }

    #[test]
    fn l2_extracts_two_volatile_accesses_in_firmware() {
        // The FIRMWARE_IR fixture now contains: a polling read of
        // STATUS, a write to DATA, and a bit-field RMW on CTRL.
        // L3+L4 must produce 3 accesses (polling read, DATA write,
        // CTRL write — the RMW load is collapsed).
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(FIRMWARE_IR, &opts).unwrap();
        let f = &ext.functions[0];
        assert_eq!(f.accesses.len(), 3, "{:#?}", f.accesses);
        assert_eq!(f.accesses[0].kind, AccessKind::Read);
        assert_eq!(f.accesses[0].register, "STATUS");
        assert_eq!(f.accesses[1].kind, AccessKind::Write);
        assert_eq!(f.accesses[1].register, "DATA");
        assert_eq!(f.accesses[2].kind, AccessKind::Write);
        assert_eq!(f.accesses[2].register, "CTRL");
    }

    #[test]
    fn l2_emits_no_accesses_without_register_map() {
        // Slice-2.a / L1 behaviour preserved when no map is given.
        let ext = extract_from_ir_text(FIRMWARE_IR).unwrap();
        let f = &ext.functions[0];
        assert!(f.accesses.is_empty());
        assert!(f.automaton_ctxdsl.is_none());
        assert!(ext.warnings.is_empty());
    }

    #[test]
    fn l3_synthesises_automaton_with_polling_loop_state() {
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(FIRMWARE_IR, &opts).unwrap();
        let f = &ext.functions[0];
        let ctxdsl = f.automaton_ctxdsl.as_ref().expect("automaton emitted");
        assert!(ctxdsl.contains("state S0 initial"));
        assert!(ctxdsl.contains("state Loop0"));
        assert!(ctxdsl.contains("rd_status_tx_busy"));
        assert!(ctxdsl.contains("wr_data_byte"));
    }

    #[test]
    fn l2_inttoptr_literal_matches_against_register_window() {
        // Example 2 from the plan: `*(volatile uint32_t *)0x40010004 = 1`
        // → a `store volatile` to an `inttoptr` literal landing inside
        //   STATUS register's [base + 4, base + 8) byte window.
        let ir = r#"source_filename = "f.c"
define void @clear_status() {
  %1 = inttoptr i64 1073807364 to ptr
  store volatile i32 1, ptr %1, align 4
  ret void
}
"#;
        // 0x40010004 = 1073807364.
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        let f = &ext.functions[0];
        assert_eq!(f.accesses.len(), 1);
        assert_eq!(f.accesses[0].kind, AccessKind::Write);
        assert_eq!(f.accesses[0].register, "STATUS");
    }

    #[test]
    fn l2_inttoptr_outside_register_window_emits_unknown_warning() {
        // Address 0x60000000 is outside the UART_LITE peripheral's
        // [0x40010000, 0x4001000C) window. The access must NOT be
        // emitted and an UnknownAccessor warning must surface.
        let ir = r#"define void @stray() {
  %1 = inttoptr i64 1610612736 to ptr
  store volatile i32 1, ptr %1, align 4
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        assert!(ext.functions[0].accesses.is_empty());
        assert!(
            ext.warnings
                .iter()
                .any(|w| matches!(w, LlvmExtractWarning::UnknownAccessor { .. }))
        );
    }

    #[test]
    fn l2_dynamic_gep_index_surfaces_unresolved_warning() {
        // A GEP with a dynamic (SSA) index can't be resolved
        // statically. Phase L2 emits UnresolvedPointer and drops
        // the access.
        let ir = r#"%struct.UART_TypeDef = type { i32, i32, i32 }
@UART = external constant ptr, align 8
define void @bad_index(i32 %0) {
  %2 = load ptr, ptr @UART, align 8
  %3 = getelementptr inbounds %struct.UART_TypeDef, ptr %2, i32 0, i32 %0
  %4 = load volatile i32, ptr %3, align 4
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        assert!(ext.functions[0].accesses.is_empty());
        assert!(
            ext.warnings
                .iter()
                .any(|w| matches!(w, LlvmExtractWarning::UnresolvedPointer { .. })),
            "got warnings: {:?}",
            ext.warnings
        );
    }

    #[test]
    #[ignore = "manual: a debug dump; run with --ignored --nocapture"]
    fn debug_dump_plan() {
        let m = crate::llvm_ir::parse_module(FIRMWARE_IR).unwrap();
        let f = &m.functions[0];
        let ctx = build_resolver_ctx(f, HashMap::new());
        let plan = build_extraction_plan(f, &uart_register_map(), &ctx);
        eprintln!("polling_loop_loads: {:?}", plan.polling_loop_loads);
        eprintln!("skip_blocks: {:?}", plan.skip_blocks);
        eprintln!("rmw_consumed_loads: {:?}", plan.rmw_consumed_loads);
        eprintln!("rmw_store_field: {:?}", plan.rmw_store_field);
    }

    #[test]
    #[ignore = "manual: a debug dump; run with --ignored --nocapture"]
    fn debug_dump_firmware_ir() {
        let m = crate::llvm_ir::parse_module(FIRMWARE_IR).unwrap();
        for f in &m.functions {
            eprintln!("Function: {}", f.name);
            for bb in &f.basic_blocks {
                eprintln!("  Block {} (preds: {:?})", bb.label, bb.predecessors);
                for (i, instr) in bb.instructions.iter().enumerate() {
                    eprintln!("    [{i}] {instr:?}");
                }
                eprintln!("    term: {:?}", bb.terminator);
            }
        }
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(FIRMWARE_IR, &opts).unwrap();
        eprintln!("Accesses: {:#?}", ext.functions[0].accesses);
        eprintln!("Warnings: {:#?}", ext.warnings);
    }

    // ----------------------------------------------------------------
    // Phase L3 / L4 tests — polling-loop + bit-field RMW.
    // ----------------------------------------------------------------

    #[test]
    fn l3_polling_loop_detected_in_firmware_ir() {
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(FIRMWARE_IR, &opts).unwrap();
        let status_read = &ext.functions[0].accesses[0];
        assert_eq!(status_read.kind, AccessKind::Read);
        assert_eq!(status_read.register, "STATUS");
        assert_eq!(
            status_read.flow,
            AccessFlow::PollingLoop,
            "polling-loop read must carry PollingLoop flow"
        );
    }

    #[test]
    fn l4_bitfield_rmw_collapses_to_single_write() {
        // The IR carries a load-modify-store sequence on CTRL:
        //   %16 = load volatile i32 ptr %15
        //   %17 = and i32 %16, -2
        //   %18 = or i32 %17, 1
        //   store volatile i32 %18, ptr %15
        // Phase L4 must drop the load and emit ONE write to
        // CTRL.tx_start (bit 0).
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(FIRMWARE_IR, &opts).unwrap();
        let ctrl_accesses: Vec<&RegisterAccess> = ext.functions[0]
            .accesses
            .iter()
            .filter(|a| a.register == "CTRL")
            .collect();
        assert_eq!(ctrl_accesses.len(), 1, "RMW must collapse to one access");
        assert_eq!(ctrl_accesses[0].kind, AccessKind::Write);
        assert_eq!(ctrl_accesses[0].field.as_deref(), Some("tx_start"));
    }

    #[test]
    fn l3_parity_with_slice2c_on_firmware_ir() {
        // The full parity gate: the LLVM backend must produce the
        // canonical S0 → Loop0 → S1 → S2 → S3 shape, same labels as
        // slice 2.c's `synthesise_automaton_ctxdsl` would.
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(FIRMWARE_IR, &opts).unwrap();
        let f = &ext.functions[0];
        assert_eq!(f.accesses.len(), 3, "{:#?}", f.accesses);
        let ctxdsl = f.automaton_ctxdsl.as_ref().unwrap();
        // Loop state, post-loop state, two write states.
        for marker in [
            "state S0 initial",
            "state Loop0",
            "state S1",
            "state S2",
            "state S3",
            "transition S0 -> Loop0 on label rd_status_tx_busy",
            "transition Loop0 -> Loop0 on label rd_status_tx_busy",
            "transition Loop0 -> S1 on label rd_status_tx_busy",
            "transition S1 -> S2 on label wr_data_byte",
            "transition S2 -> S3 on label wr_ctrl_tx_start",
        ] {
            assert!(
                ctxdsl.contains(marker),
                "missing marker `{marker}` in:\n{ctxdsl}"
            );
        }
        assert!(
            !ctxdsl.contains("state S4"),
            "L3+L4 collapse must yield no S4"
        );
    }

    #[test]
    fn l4_bitfield_picks_field_from_or_bits_not_lowest_bit() {
        // A bit-field write that targets a field at bits [4, 7] (not
        // [0, 0]) must select that field, not fall back to the
        // lowest-bit field.
        let mut rm = uart_register_map();
        // Append a higher-bit field to CTRL.
        rm.registers[0].fields.push(Field {
            name: "tx_mode".to_string(),
            bits: [4, 7],
            sv_signal: None,
            c_accessor: Some("UART->CTRL.bit.tx_mode".to_string()),
            description: None,
        });
        // IR: read CTRL, AND with 0xFFFFFF0F (clear bits 4-7), OR with
        // 0x00000020 (set bit 5), store back.
        let ir = r#"%struct.UART_TypeDef = type { i32, i32, i32 }
@UART = external constant ptr, align 8
define void @set_mode() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 0
  %3 = load volatile i32, ptr %2, align 4
  %4 = and i32 %3, -241
  %5 = or i32 %4, 32
  store volatile i32 %5, ptr %2, align 4
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(rm),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        let f = &ext.functions[0];
        assert_eq!(f.accesses.len(), 1);
        assert_eq!(f.accesses[0].kind, AccessKind::Write);
        assert_eq!(f.accesses[0].register, "CTRL");
        assert_eq!(f.accesses[0].field.as_deref(), Some("tx_mode"));
    }

    // ----------------------------------------------------------------
    // Phase L5 tests — interprocedural call-graph walk.
    // ----------------------------------------------------------------

    #[test]
    fn l5_inlines_callee_accesses_at_the_call_site() {
        // Example 4 from the plan: a helper function holds the
        // polling loop, the caller chains DATA write + CTRL write.
        // The IR has separate `define` blocks for both functions;
        // phase L5 inlines the callee's accesses at the call site.
        let ir = r#"%struct.UART_TypeDef = type { i32, i32, i32 }
@UART = external constant ptr, align 8

define void @uart_wait_idle() {
1:
  %2 = load ptr, ptr @UART, align 8
  %3 = getelementptr inbounds %struct.UART_TypeDef, ptr %2, i32 0, i32 1
  %4 = load volatile i32, ptr %3, align 4
  %5 = and i32 %4, 1
  %6 = icmp ne i32 %5, 0
  br i1 %6, label %7, label %8

7:
  br label %1

8:
  ret void
}

define void @uart_send(i8 %0) {
  call void @uart_wait_idle()
  %2 = load ptr, ptr @UART, align 8
  %3 = getelementptr inbounds %struct.UART_TypeDef, ptr %2, i32 0, i32 2
  store volatile i8 %0, ptr %3, align 4
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        // Two functions in the module — the caller (uart_send) and
        // the callee (uart_wait_idle). Each has its own summary.
        let send = ext
            .functions
            .iter()
            .find(|f| f.name == "uart_send")
            .expect("uart_send extracted");
        // After L5 inlining: the polling-loop STATUS read (from
        // uart_wait_idle) + the DATA write (from uart_send body).
        assert_eq!(send.accesses.len(), 2, "{:#?}", send.accesses);
        assert_eq!(send.accesses[0].kind, AccessKind::Read);
        assert_eq!(send.accesses[0].register, "STATUS");
        assert_eq!(send.accesses[0].flow, AccessFlow::PollingLoop);
        assert_eq!(send.accesses[1].kind, AccessKind::Write);
        assert_eq!(send.accesses[1].register, "DATA");
    }

    #[test]
    fn l5_recursive_call_breaks_with_warning_no_infinite_loop() {
        let ir = r#"define void @loops() {
  call void @loops()
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        // The walker must not infinite-loop; it must emit a
        // RecursiveCall warning when it re-enters the same function.
        assert!(
            ext.warnings
                .iter()
                .any(|w| matches!(w, LlvmExtractWarning::RecursiveCall { function } if function == "loops")),
            "got warnings: {:?}",
            ext.warnings
        );
    }

    // ----------------------------------------------------------------
    // Phase L6 tests — @mununu_isr annotation + async composition.
    // ----------------------------------------------------------------

    // ----------------------------------------------------------------
    // Phase L5.5 test — pointer-parameter alias tracking.
    // ----------------------------------------------------------------

    #[test]
    fn l5_5_pointer_parameter_alias_tracked_through_alloca() {
        // The caller passes @UART to a helper that dereferences its
        // parameter. Phase L5.5 chases the callee's alloca-store-load
        // round-trip back to the parameter SSA, then looks up the
        // parameter binding from the caller's argument. The STATUS
        // read inside the helper must surface in the caller's
        // access list.
        let ir = r#"%struct.UART_TypeDef = type { i32, i32, i32 }
@UART = external constant ptr, align 8

define internal void @uart_read_status(ptr noundef %0) {
  %2 = alloca ptr, align 8
  store ptr %0, ptr %2, align 8
  %3 = load ptr, ptr %2, align 8
  %4 = getelementptr inbounds %struct.UART_TypeDef, ptr %3, i32 0, i32 1
  %5 = load volatile i32, ptr %4, align 4
  ret void
}

define void @uart_send() {
  %1 = load ptr, ptr @UART, align 8
  call void @uart_read_status(ptr noundef %1)
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        let send = ext
            .functions
            .iter()
            .find(|f| f.name == "uart_send")
            .expect("uart_send extracted");
        // The STATUS read from inside uart_read_status must surface
        // in uart_send's accesses — L5.5 followed the parameter
        // alias through the alloca round-trip.
        assert!(
            send.accesses
                .iter()
                .any(|a| a.register == "STATUS" && a.kind == AccessKind::Read),
            "L5.5 must lift the parameter-aliased STATUS read into the caller's accesses; got: {:#?}",
            send.accesses
        );
    }

    #[test]
    fn l6_detects_isr_annotation_in_source() {
        let source = r#"
/* main thread driver */
void uart_send(uint8_t byte) {}

/**
 * @mununu_isr
 */
void UART_IRQHandler(void) {}
"#;
        let isrs = detect_isr_functions(source);
        assert!(isrs.contains("UART_IRQHandler"), "got: {isrs:?}");
        assert!(!isrs.contains("uart_send"));
    }

    #[test]
    fn l6_marks_isr_function_in_summary() {
        let ir = r#"@UART = external constant ptr, align 8
define void @uart_send() {
  ret void
}
define void @UART_IRQHandler() {
  ret void
}
"#;
        let source = r#"
void uart_send(uint8_t byte) {}

/**
 * @mununu_isr
 */
void UART_IRQHandler(void) {}
"#;
        let ext = extract_from_ir_text_with_options_and_source(
            ir,
            source,
            &LlvmExtractOptions::default(),
        )
        .unwrap();
        let isr = ext
            .functions
            .iter()
            .find(|f| f.name == "UART_IRQHandler")
            .unwrap();
        assert!(isr.is_isr);
        let send = ext
            .functions
            .iter()
            .find(|f| f.name == "uart_send")
            .unwrap();
        assert!(!send.is_isr);
    }

    #[test]
    fn l6_emits_async_composition_when_isr_present() {
        let ir = r#"@UART = external constant ptr, align 8
%struct.UART_TypeDef = type { i32, i32, i32 }
define void @uart_send() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 2
  store volatile i8 0, ptr %2, align 4
  ret void
}
define void @UART_IRQHandler() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 1
  %3 = load volatile i32, ptr %2, align 4
  ret void
}
"#;
        let source = r#"
void uart_send(uint8_t byte) {}

/**
 * @mununu_isr
 */
void UART_IRQHandler(void) {}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options_and_source(ir, source, &opts).unwrap();
        let comp = ext
            .composition_ctxdsl
            .as_ref()
            .expect("composition emitted when ISR present");
        assert!(comp.contains("compositions"));
        assert!(comp.contains("asynchronous"));
        assert!(comp.contains("Uart_send"));
        assert!(comp.contains("UART_IRQHandler") || comp.contains("Uart_irqhandler"));
        assert!(comp.contains("// ISR"));
    }

    #[test]
    fn l6_no_composition_when_no_isr_annotation() {
        let ir = r#"define void @uart_send() {
  ret void
}
"#;
        let source = "void uart_send(uint8_t byte) {}\n";
        let ext = extract_from_ir_text_with_options_and_source(
            ir,
            source,
            &LlvmExtractOptions::default(),
        )
        .unwrap();
        assert!(ext.composition_ctxdsl.is_none());
    }

    // ----------------------------------------------------------------
    // Phase L7 tests — multi-entry driver composition.
    // ----------------------------------------------------------------

    fn two_entry_driver_ir() -> &'static str {
        r#"@UART = external constant ptr, align 8
%struct.UART_TypeDef = type { i32, i32, i32 }
define void @uart_send() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 2
  store volatile i8 0, ptr %2, align 4
  ret void
}
define void @uart_recv() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 1
  %3 = load volatile i32, ptr %2, align 4
  ret void
}
"#
    }

    #[test]
    fn l7_driver_off_by_default() {
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(two_entry_driver_ir(), &opts).unwrap();
        assert!(ext.driver_ctxdsl.is_none());
    }

    #[test]
    fn l7_driver_emits_when_two_entries_and_driver_mode_on() {
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            driver_mode: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(two_entry_driver_ir(), &opts).unwrap();
        let driver = ext.driver_ctxdsl.as_ref().expect("Driver emitted");
        for marker in [
            "automaton Driver",
            "state Idle initial",
            "state Calling_uart_send",
            "state Calling_uart_recv",
            "Idle -> Calling_uart_send on label call_uart_send",
            "Calling_uart_send -> Idle on label return_uart_send",
            "Idle -> Calling_uart_recv on label call_uart_recv",
            "Calling_uart_recv -> Idle on label return_uart_recv",
        ] {
            assert!(driver.contains(marker), "missing `{marker}` in:\n{driver}");
        }
    }

    #[test]
    fn l7_driver_suppressed_with_only_one_entry() {
        let single = r#"define void @uart_send() {
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            driver_mode: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(single, &opts).unwrap();
        // Only one function and it has no accesses → no automaton →
        // no driver (would be a single-entry dispatch, pointless).
        assert!(ext.driver_ctxdsl.is_none());
    }

    #[test]
    fn l7_driver_excludes_isr_functions() {
        let ir = r#"@UART = external constant ptr, align 8
%struct.UART_TypeDef = type { i32, i32, i32 }
define void @uart_send() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 2
  store volatile i8 0, ptr %2, align 4
  ret void
}
define void @uart_recv() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 1
  %3 = load volatile i32, ptr %2, align 4
  ret void
}
define void @UART_IRQHandler() {
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 1
  %3 = load volatile i32, ptr %2, align 4
  ret void
}
"#;
        let source = r#"
void uart_send(uint8_t byte) {}
void uart_recv(uint8_t *out) {}

/**
 * @mununu_isr
 */
void UART_IRQHandler(void) {}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            driver_mode: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options_and_source(ir, source, &opts).unwrap();
        let driver = ext.driver_ctxdsl.as_ref().expect("Driver emitted");
        assert!(driver.contains("Calling_uart_send"));
        assert!(driver.contains("Calling_uart_recv"));
        // The ISR is composed *asynchronously* via the L6
        // composition block, not dispatched by the Driver.
        assert!(!driver.contains("UART_IRQHandler"));
        assert!(ext.composition_ctxdsl.is_some());
    }

    #[test]
    fn l5_external_call_falls_through_silently() {
        // Calls to external symbols (not defined in this module) are
        // not inlined and produce no warning — they're the chaotic-
        // stub equivalent of "we have no body to walk."
        let ir = r#"@UART = external constant ptr, align 8
%struct.UART_TypeDef = type { i32, i32, i32 }
define void @uart_send() {
  call void @printf(ptr %0)
  %2 = load ptr, ptr @UART, align 8
  %3 = getelementptr inbounds %struct.UART_TypeDef, ptr %2, i32 0, i32 0
  store volatile i32 1, ptr %3, align 4
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        let f = &ext.functions[0];
        // One access — the CTRL write that comes after the printf
        // call. The external call doesn't produce a recursion or
        // unknown warning.
        assert_eq!(f.accesses.len(), 1);
        assert_eq!(f.accesses[0].register, "CTRL");
        assert!(
            !ext.warnings
                .iter()
                .any(|w| matches!(w, LlvmExtractWarning::RecursiveCall { .. }))
        );
    }

    #[test]
    fn l2_round_trips_warnings_through_serde() {
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(FIRMWARE_IR, &opts).unwrap();
        let json = serde_json::to_string_pretty(&ext).unwrap();
        let parsed: LlvmExtraction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.functions.len(), 1);
        assert_eq!(parsed.functions[0].accesses.len(), 3);
    }

    #[test]
    fn lookup_state_marker_name_handles_clang_arg_shapes() {
        let mut consts = BTreeMap::new();
        consts.insert(".str".to_string(), "Polling".to_string());
        consts.insert(".str.1".to_string(), "Ready".to_string());

        // Plain `ptr noundef @.str` shape (clang -O0 default).
        assert_eq!(
            lookup_state_marker_name("ptr noundef @.str", &consts),
            Some("Polling".to_string())
        );
        // `@.str.1` with dot in the name.
        assert_eq!(
            lookup_state_marker_name("ptr noundef @.str.1", &consts),
            Some("Ready".to_string())
        );
        // Inline `getelementptr` constexpr shape: still finds the
        // global because the global name has no `(` inside it.
        assert_eq!(
            lookup_state_marker_name(
                "ptr noundef getelementptr inbounds ([8 x i8], ptr @.str, i32 0, i32 0)",
                &consts
            ),
            Some("Polling".to_string())
        );
        // Missing global → None.
        assert_eq!(
            lookup_state_marker_name("ptr noundef @.missing", &consts),
            None
        );
        // Arg with no `@` at all → None.
        assert_eq!(lookup_state_marker_name("i32 42", &consts), None);
    }

    #[test]
    fn mununu_state_markers_rename_synthesised_automaton_states() {
        // Minimal IR: two volatile writes to CTRL.tx_start, separated
        // by a `__mununu_state` marker. The marker before the second
        // write should rename the state between the two accesses.
        let ir = r#"; ModuleID = 'state_marker_demo.c'
source_filename = "state_marker_demo.c"
target triple = "x86_64-apple-macosx26.0.0"

@UART = external constant ptr, align 8
@.str = private unnamed_addr constant [8 x i8] c"Polling\00", align 1
@.str.1 = private unnamed_addr constant [6 x i8] c"Ready\00", align 1

%struct.UART_TypeDef = type { %union.anon }

declare void @__mununu_state(ptr noundef)

define void @demo() {
  call void @__mununu_state(ptr noundef @.str)
  %1 = load ptr, ptr @UART, align 8
  %2 = getelementptr inbounds %struct.UART_TypeDef, ptr %1, i32 0, i32 0
  store volatile i32 1, ptr %2, align 4
  call void @__mununu_state(ptr noundef @.str.1)
  %3 = load ptr, ptr @UART, align 8
  %4 = getelementptr inbounds %struct.UART_TypeDef, ptr %3, i32 0, i32 0
  store volatile i32 1, ptr %4, align 4
  ret void
}
"#;
        let opts = LlvmExtractOptions {
            register_map: Some(uart_register_map()),
            synthesize_automaton: true,
            ..Default::default()
        };
        let ext = extract_from_ir_text_with_options(ir, &opts).unwrap();
        assert_eq!(ext.functions.len(), 1);
        let f = &ext.functions[0];
        assert_eq!(f.accesses.len(), 2, "two writes expected, markers consumed");
        assert_eq!(
            f.accesses[0].source_state_hint.as_deref(),
            Some("Polling"),
            "first access should be tagged with 'Polling'"
        );
        assert_eq!(
            f.accesses[1].source_state_hint.as_deref(),
            Some("Ready"),
            "second access should be tagged with 'Ready'"
        );
        let automaton = f.automaton_ctxdsl.as_ref().expect("automaton synthesised");
        assert!(
            automaton.contains("state Polling initial"),
            "initial state should be 'Polling'; got:\n{automaton}"
        );
        assert!(
            automaton.contains("state Ready;"),
            "intermediate state should be 'Ready'; got:\n{automaton}"
        );
        assert!(
            automaton.contains("Polling -> Ready"),
            "first transition should be Polling -> Ready; got:\n{automaton}"
        );
        assert!(
            automaton.contains("Ready -> S2"),
            "second transition should be Ready -> S2; got:\n{automaton}"
        );
    }
}
