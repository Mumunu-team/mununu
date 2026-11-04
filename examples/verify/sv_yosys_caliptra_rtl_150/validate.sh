#!/usr/bin/env bash
# validate.sh — Caliptra-RTL #150 SoC interface boot FSM (Phase A.3 sanity).
#
# Runs the sv-yosys → BTOR2 pipeline on chipsalliance/caliptra-rtl's
# `soc_ifc_boot_fsm_pre_fix.sv` and asserts that:
#
#   (a) translation completes (no Yosys / bit-blast errors),
#   (b) the auto-partition + sidecar abstractions land the design under
#       the BTOR2 bit-blaster's MAX_STATE_BITS = 20 cap with a
#       state-count well below 2^19 (the raw bit width before
#       abstraction). Before Phase A.3 the pipeline could not finish on
#       this design in 22 minutes / 4.6 GB RSS on a release build —
#       see docs/design/caliptra-abstraction-analysis.md §2.2.
#
# This is a **structural sanity check**, not a property-verification
# claim. The mu-calculus evaluator does return verdicts on this design
# (set EVAL=1 to exercise that path under a 5-minute cap), but the
# verdicts are **vacuous** — formula atoms like `boot_fsm_ps == 5` do
# not yet bind to state-cell valuations on the bit-blasted CLTS, so
# every state "satisfies" the negation by default. The CWE-1245
# UNDEF-state reachability finding cannot be cited as a verdict
# claim until state-cell-aware predicate binding ships (separate
# scope from Phase A.3).
#
# Per docs/policies/claims-integrity.md this fixture demonstrates an
# auto-extraction structural milestone on a real upstream design.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu binary not found at ${MUNUNU}" >&2
  echo "                build it first with: cargo build -p mununu-cli" >&2
  exit 2
fi

# yosys + sv2v are required for this fixture. They are not bundled with
# the mununu repo; the dev container installs them.
for tool in yosys; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "validate.sh: required tool '${tool}' not on PATH" >&2
    exit 2
  fi
done

cd "${THIS_DIR}"

# Run the pipeline. The sidecar + additional SV package files are
# passed via repeated `--sidecar` flags (the CLI loader resolves them
# from the same directory tree).
echo "==> mununu context summarize --adapter sv-yosys"
echo "    cwd: source/"
echo "    primary: soc_ifc_boot_fsm_pre_fix.sv"
echo "    sidecar (auto-loaded): soc_ifc_boot_fsm_pre_fix.mununu.json"
echo "    additional sources: soc_ifc_pkg.sv, soc_ifc_reg_pkg.sv"

OUTPUT="$(
  cd source && "${MUNUNU}" context summarize \
    --adapter sv-yosys \
    --sidecar soc_ifc_pkg.sv \
    --sidecar soc_ifc_reg_pkg.sv \
    soc_ifc_boot_fsm_pre_fix.sv 2>&1 || true
)"

echo "==> adapter output (filtered)"
echo "${OUTPUT}" | grep -E "Translated|adapter warning|auto-partition" || true

# Parse out "Translated SystemVerilog: N signals, S states, P properties"
TRANSLATED_LINE="$(echo "${OUTPUT}" | grep -oE 'Translated SystemVerilog:[^,]+, [0-9]+ states' || true)"
if [[ -z "${TRANSLATED_LINE}" ]]; then
  echo "validate.sh: pipeline did not emit a Translated line — pipeline failed before bit-blast" >&2
  echo "${OUTPUT}" | tail -20 >&2
  exit 1
fi

STATES="$(echo "${TRANSLATED_LINE}" | grep -oE '[0-9]+ states' | grep -oE '[0-9]+')"
echo "==> bit-blaster reported ${STATES} states"

# Headline assertion: auto-partition + sidecar must land below the
# Phase 1.6 ceiling. The raw bit count is 2^19 = 524288 explicit
# states before any abstraction; we assert the realised count is at
# least an order of magnitude below that.
if [[ "${STATES}" -ge 100000 ]]; then
  echo "validate.sh: FAIL — state count ${STATES} is too close to the raw 2^19 = 524288 ceiling" >&2
  echo "             auto-partition + sidecar abstraction did not deliver the expected reduction" >&2
  exit 1
fi

# Auto-COI may or may not emit drop warnings on this fixture — the
# user-curated sidecar already mentions every relevant signal, so
# user-wins-on-collision keeps the partition from re-classifying
# anything. We document the absence here rather than asserting on it.
COI_DROPS="$(echo "${OUTPUT}" | grep -c "auto-partition: " || true)"
echo "==> auto-partition (live) drop warnings: ${COI_DROPS}"
if [[ "${COI_DROPS}" -eq 0 ]]; then
  echo "    (expected — the sidecar lists every signal the auto-COI"
  echo "     classifier would otherwise drop; user wins on collision)"
fi

echo "==> PASS: structural sanity check completed under the threshold"
echo "    States: ${STATES} (raw 2^19 = 524288 → ${STATES} after"
echo "    abstraction; auto-partition is wired but the sidecar's"
echo "    explicit declarations are doing the heavy lifting on this"
echo "    fixture per the user-wins-on-collision rule)."

# Optional: exercise the mu-calculus evaluator. Returns vacuous
# verdicts today (state-cell-aware predicate binding is separate from
# Phase A.3); opt-in via EVAL=1 to confirm the eval path completes
# within the architecture-doc runtime gate.
if [[ "${EVAL:-0}" -eq 1 ]]; then
  echo ""
  echo "==> EVAL=1: exercising mu-calculus evaluator (cap 300s per property)"
  for FORMULA in safety_all_states_have_successors no_undef_reachable boot_idle_reachable; do
    echo "  - ${FORMULA}"
    EVAL_OUT="$(
      cd source && timeout 300 "${MUNUNU}" context eval \
        --adapter sv-yosys \
        --sidecar soc_ifc_pkg.sv \
        --sidecar soc_ifc_reg_pkg.sv \
        --formula "${FORMULA}" \
        --automaton Circuit \
        soc_ifc_boot_fsm_pre_fix.sv 2>&1 || true
    )"
    VERDICT_LINE="$(echo "${EVAL_OUT}" | grep -E "(States|Initial states) satisfying:" | head -2)"
    if [[ -z "${VERDICT_LINE}" ]]; then
      echo "      (no verdict — likely timed out at 5 minutes)"
    else
      echo "${VERDICT_LINE}" | sed 's/^/      /'
    fi
  done
  echo "    NOTE: verdicts are currently vacuous (predicate-binding gap)."
fi
