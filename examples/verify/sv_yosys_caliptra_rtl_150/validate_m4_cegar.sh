#!/usr/bin/env bash
# validate_m4_cegar.sh — M.4 milestone: full automated predicate-abstraction
# CEGAR on Caliptra's `soc_ifc_boot_fsm`, demonstrating a sound, definite,
# pre/post-distinguishing verdict on real industrial RTL (§Phase 11 slot 7).
#
# Contrast with `validate.sh` in this directory, which is the Phase-A.3
# *structural* sanity check (bit-blast reduction). This script exercises
# the R.5 predicate-cube CEGAR path end-to-end and is the M.4 milestone
# proper.
#
# The hazard. `soc_ifc_boot_fsm` holds its present state in a 3-bit
# `boot_fsm_state_e` register with five legal encodings (0..4); the
# `unique casez` enumerates exactly those five. The `pre_fix` variant has
# NO `default` arm, so the encodings {5,6,7} — admissible by the type but
# unhandled — latch (CWE-1245). The `post_fix` variant adds a `default`
# that routes every other encoding back to a defined state.
#
# The property. `<> (p5 || p6 || p7)` over the predicate cube
# {p5,p6,p7} = {boot_fsm_ns ∈ {5,6,7}} — "the next-state register can
# transition into an undefined encoding". Under `setundef -anyconst` (the
# register's power-up value is unconstrained):
#   - pre_fix  : the undefined encoding is DEFINITELY reachable/latching
#                (every cube cell decided, ⊥=0) → CWE-1245 hazard present.
#   - post_fix : the undefined encoding is NO LONGER A DEFINITE hazard —
#                ≥1 cell is KleeneBot (⊥) → the default-arm fix removes the
#                *definite* latch.
# The sound pre/post difference (definite-hazard → indeterminate) is the
# milestone evidence.
#
# SOUNDNESS (revised 2026-06-22, IR-track P3.4). The original criterion
# claimed post_fix is "UNSATISFIABLE / proven unreachable / fix verified".
# That was UNSOUND: it relied on the Skolem-collapsed `<>`→`[]` diamond
# (one shared `step` label ⇒ "all step-successors satisfy") which ignored
# the cube's over-approximating may-self-loops. Under the corrected
# EXISTENTIAL `Control::All` diamond, post_fix is genuinely KleeneBot, not
# definite-safe — and rightly so: post_fix's `default: boot_fsm_ns =
# boot_fsm_ps` HOLDS the undefined encoding, escaping only via the reset
# window (`boot_fsm_ps <= arc_IDLE ? BOOT_IDLE : boot_fsm_ns`). So the
# {p5,p6,p7} cube CANNOT soundly PROVE the fixed FSM safe; it soundly shows
# the definite hazard is gone. `--must-edge-inference smt-per-target`
# proves must-edges with a Z3 ∀∀ query (no sampling). Proving full safety
# (post_fix definite-safe) would require a finer abstraction / CEGAR to
# convergence — out of scope here. RTL-level counterexample replay under a
# cycle-accurate simulator remains the empirical step (deferred).
#
# Per §10.2: if any stage fails, STOP and produce a blocker note rather
# than silently retrying / hand-authoring around it.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
# Transient BTOR2/CEGAR artefacts go to a tmp dir, NOT under examples/.
# The full Caliptra BTOR2 busts the bit-blast state-bit cap by design
# (M.4 decides it via predicate-cube CEGAR, not the full R.2 lift), so
# leaving it under examples/ would trip the `btor2_kmts_lift_sweep` test
# which globs `examples/**/*.btor2`. mktemp + EXIT-trap keeps it clean.
OUT="$(mktemp -d -t mununu-m4-XXXXXX)"
trap 'rm -rf "${OUT}"' EXIT

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate_m4_cegar.sh: mununu binary not found at ${MUNUNU}" >&2
  echo "                      build it first with: cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in yosys sv2v; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "validate_m4_cegar.sh: required tool '${tool}' not on PATH" >&2
    exit 2
  fi
done


echo "=== M.4 — Caliptra soc_ifc_boot_fsm predicate-abstraction CEGAR ==="
echo "upstream: chipsalliance/caliptra-rtl @ (see source/, issue #150 bug/fix pair)"
echo "property: <> (p5 || p6 || p7)   [boot_fsm_ns transitions into an unmatched encoding]"
echo

PREDS=(--predicate p5:boot_fsm_ns=5 --predicate p6:boot_fsm_ns=6 --predicate p7:boot_fsm_ns=7
       --init-values 'boot_fsm_ns=0,1,2,3,4,5,6,7' --must-edge-inference smt-per-target)

# Generate BTOR2 + run CEGAR for one variant; echoes the T cell count.
run_variant() {
  local variant="$1"
  local v="${OUT}/${variant}.v"
  local b="${OUT}/${variant}.btor2"
  sv2v -I"${SRC}" \
    "${SRC}/soc_ifc_pkg.sv" "${SRC}/soc_ifc_reg_pkg.sv" \
    "${SRC}/caliptra_2ff_sync.sv" "${SRC}/soc_ifc_boot_fsm_${variant}.sv" \
    > "${v}" 2>"${OUT}/${variant}.sv2v.err"
  yosys -q -p "read_verilog -sv ${v}; hierarchy -check -top soc_ifc_boot_fsm; proc; \
               flatten; async2sync; dffunmap; setundef -anyconst; write_btor ${b}" \
    2>"${OUT}/${variant}.yosys.err"
  LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}" "${MUNUNU}" btor2 cegar "${b}" \
    --formula '<> (p5 || p6 || p7)' "${PREDS[@]}" \
    > "${OUT}/${variant}.cegar.out" 2>"${OUT}/${variant}.cegar.err"
  cat "${OUT}/${variant}.cegar.out"
}

echo "--- pre_fix (bug-bearing: no default arm) ---"
run_variant pre_fix
echo
echo "--- post_fix (fixed: default routes unmatched → defined) ---"
run_variant post_fix
echo

# Extract the True / KleeneBot cell counts from each
# "verdict cells: T=N F=M ⊥=K" line.
t_count() { grep -E "verdict cells:" "$1" | sed -E 's/.*T=([0-9]+).*/\1/'; }
bot_count() { grep -E "verdict cells:" "$1" | sed -E 's/.*⊥=([0-9]+).*/\1/'; }
PRE_T="$(t_count "${OUT}/pre_fix.cegar.out")"
POST_T="$(t_count "${OUT}/post_fix.cegar.out")"
PRE_BOT="$(bot_count "${OUT}/pre_fix.cegar.out")"
POST_BOT="$(bot_count "${OUT}/post_fix.cegar.out")"

echo "pre_fix : T=${PRE_T} ⊥=${PRE_BOT}"
echo "post_fix: T=${POST_T} ⊥=${POST_BOT}"

if [[ -z "${PRE_T}" || -z "${POST_T}" || -z "${PRE_BOT}" || -z "${POST_BOT}" ]]; then
  echo "FAIL: could not parse a verdict from one of the CEGAR runs" >&2
  exit 1
fi
# SOUND done-criterion (IR-track P3.4, 2026-06-22). The original criterion
# `post_fix T==0` ("fix verified — hazard unreachable") was UNSOUND: it
# relied on the Skolem-collapsed `<>`→`[]` diamond, which ignored the cube's
# over-approximating may-self-loops. Under the corrected EXISTENTIAL
# `Control::All` diamond, the {p5,p6,p7} cube is too coarse to PROVE the
# post_fix FSM safe — and genuinely so: post_fix's `default: boot_fsm_ns =
# boot_fsm_ps` HOLDS the undefined encoding, and the register only escapes via
# `boot_fsm_ps <= arc_IDLE ? BOOT_IDLE : boot_fsm_ns` (the reset window). So
# post_fix's undefined-encoding cells are genuinely KleeneBot (may latch, may
# escape), not definite-safe.
#
# The milestone therefore demonstrates the sound pre/post DISTINCTION, not a
# "verified safe" claim:
#   pre_fix : hazard DEFINITELY present — every cell decided (⊥==0), the
#             undefined encoding reachable (T≥1).  [sound CWE-1245 detection]
#   post_fix: hazard NO LONGER DEFINITE — the fix makes ≥1 undefined-encoding
#             cell KleeneBot (⊥≥1): the FSM can escape (reset window), so the
#             definite latch is gone, but the coarse cube cannot prove full
#             safety. Proving safety would need a finer abstraction.
if [[ "${PRE_T}" -ge 1 && "${PRE_BOT}" -eq 0 && "${POST_BOT}" -ge 1 ]]; then
  echo
  echo "=== M.4 VALIDATION PASSED (sound pre/post distinction) ==="
  echo "pre_fix : undefined-encoding latch DEFINITELY present (T=${PRE_T}, ⊥=0) — CWE-1245 detected (sound)."
  echo "post_fix: undefined-encoding latch NO LONGER DEFINITE (⊥=${POST_BOT}) — the fix removes the definite hazard."
  echo "NOTE: post_fix is KleeneBot, not definite-safe — the {p5,p6,p7} cube cannot soundly PROVE the fixed"
  echo "      FSM safe (it escapes only via the reset window). 'Fix verified' would require a finer abstraction."
else
  echo "FAIL: expected pre(T>=1, ⊥==0) and post(⊥>=1); got pre(T=${PRE_T},⊥=${PRE_BOT}) post(T=${POST_T},⊥=${POST_BOT})" >&2
  exit 1
fi
