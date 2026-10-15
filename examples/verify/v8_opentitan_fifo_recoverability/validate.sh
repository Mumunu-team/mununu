#!/usr/bin/env bash
# validate.sh — V.8 recoverability showcase on OpenTitan prim_fifo_sync_cnt.
#
# Asks a branching-time question SVA cannot state, over a datapath RELATION:
#     "from every reachable fill level, can the FIFO always drain back to EMPTY?"
# Empty is not a control-FSM state — it is the relation `wptr == rptr` over the two
# wrap-pointer counters (prim_fifo_sync_cnt.sv line 63: `empty_o = wptr_wrap_cnt_q ==
# rptr_wrap_cnt_q`). So the recoverability target is RELATIONAL:
#     always_recoverable = nu Y. ((mu X. (empty || <> X)) && [] Y)   (AG EF empty)
# with empty = (wptr_wrap_cnt_q == rptr_wrap_cnt_q) — a `REG == REG` target, decided
# by the compound-good machinery (frontier b), not a `REG == VALUE` control atom.
#
# HONEST RESULT (this is the interesting part, not a bug):
#   * Small FIFO (exact engine, <= ~40 bits of pointer state): AG EF empty HOLDS —
#     from any fill level empty is reachable (drain). Definite-TRUE.
#   * Wide FIFO (beyond the exact cap, 2^21 entries = 22-bit wrap pointers): the drain to
#     empty needs the read pointer to PROGRESS up to the write pointer — a well-founded
#     descent over two INDEPENDENT counters (the RANKING class), which no bounded predicate
#     set captures, so the predicate cube alone ABSTAINS. mununu's RANKING CERTIFICATE
#     decides it anyway: over the EXACT transition it proves that from every non-empty state
#     SOME input (a read) strictly decreases the ranking δ = wptr - rptr, which — δ bounded
#     below — forces a descent to empty (Podelski-Rybalchenko, ∃-input variant, one relation,
#     no 2^|P|). So AG EF empty decides HOLDS at 2^21 depth in ~0.1s past the exact engine's reach
#     (since the 192-bit auto-cap the exact engine decides this width itself — read the JSON's engine field).
#     Contrast: the ALL-PATH variant fails here (writing keeps δ non-decreasing), which is
#     correct — the FIFO is AG EF empty but NOT AG AF empty (the env may write forever).
#
# Pedigree: prim_fifo_sync_cnt.sv + prim_count_pkg.sv are real OpenTitan RTL
# (Apache-2.0), vendored + pinned under source/ at the commit in UPSTREAM_COMMIT.txt.
# The property is a demonstration of the recoverability property class (a RELATIONAL
# datapath target) on real silicon, not a vulnerability finding.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
SRC="examples/verify/v8_opentitan_fifo_recoverability/source"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in sv2v yosys python3; do
  command -v "${tool}" >/dev/null 2>&1 || { echo "validate.sh: required tool '${tool}' not on PATH" >&2; exit 2; }
done

OUT="$(mktemp -d -t mununu-v8-XXXXXX)"
FORMULA='nu Y. ((mu X. (empty || <> X)) && [] Y)'

# sv2v the package chain + the counter module (Secure=0 non-hardened path is self-contained).
sv2v -I"${SRC}" "${SRC}/prim_count_pkg.sv" "${SRC}/prim_util_pkg.sv" \
     "${SRC}/prim_fifo_sync_cnt.sv" > "${OUT}/cnt.v" 2>"${OUT}/sv2v.err"

# Lift prim_fifo_sync_cnt at a chosen Depth to BTOR2. The two wrap-pointer counters are the
# only state; yosys does not surface their generate-scoped names, so we name the two state
# lines deterministically (cnta / cntb) — the `empty` relation `cnta == cntb` is symmetric.
# Reset is tied INACTIVE (rst_ni = 1) so recoverability rests on the DATAPATH drain (reads catching
# the write pointer), not a reset escape — the honest, harder question.
lift() {
  local depth="$1" out="$2"
  # `flatten; opt -full` inlines sv2v's identity-cast helper functions (`sv2v_cast_*`); without it yosys
  # leaves them as spurious wide free inputs that swamp the ranking certificate's input enumeration.
  yosys -q -p "read_verilog -sv ${OUT}/cnt.v; hierarchy -check -top prim_fifo_sync_cnt -chparam Depth ${depth}; \
               proc; flatten; opt -full; async2sync; dffunmap; connect -set rst_ni 1'b1; opt_clean -purge; write_btor ${out}" 2>"${OUT}/yosys.err"
  python3 - "$out" <<'PY'
import sys
p=sys.argv[1]; L=open(p).read().splitlines(); out=[]; n=[]
for ln in L:
    t=ln.split()
    if len(t)==3 and t[1]=="state":
        nm="cnta" if not n else "cntb"; n.append(nm); out.append(ln+" "+nm)
    else: out.append(ln)
open(p,"w").write("\n".join(out)+"\n")
PY
}

echo "=== V.8 — recoverability of OpenTitan prim_fifo_sync_cnt (AG EF empty, empty = wptr==rptr) ==="
echo

echo "--- small FIFO (Depth=16, exact engine) — expect HOLDS ---"
lift 16 "${OUT}/d16.btor2"
"${MUNUNU}" btor2 verify-recoverability "${OUT}/d16.btor2" --target 'cnta == cntb' 2>/dev/null | grep -iE '"verdict"'
SMALL="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/d16.btor2" --target 'cnta == cntb' 2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
echo

echo "--- wide FIFO (Depth=2^21 = 22-bit pointers) — expect HOLDS (the JSON's engine field says which engine decided) ---"
lift 2097152 "${OUT}/wide.btor2"
"${MUNUNU}" btor2 verify-recoverability "${OUT}/wide.btor2" --target 'cnta == cntb' 2>/dev/null | grep -iE '"verdict"'
WIDE="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/wide.btor2" --target 'cnta == cntb' 2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
echo

ok=1
if [[ "${SMALL}" != "holds" ]]; then echo "FAIL: small FIFO expected holds, got ${SMALL}" >&2; ok=0; fi
# Wide MUST decide HOLDS via the ranking certificate (the datapath drain is a well-founded ∃-input
# descent). 'unknown' would mean the certificate did not fire; 'violated' would be unsound (the
# property is TRUE — a read-only path always drains to empty).
if [[ "${WIDE}" != "holds" ]]; then echo "FAIL: wide FIFO expected holds (ranking certificate), got ${WIDE}" >&2; ok=0; fi

if [[ "${ok}" == "1" ]]; then
  echo "PASS — relational recoverability DECIDES HOLDS on real RTL at 2^21 depth (read the engine field: exact since the 192-bit auto-cap; the ∃-input ranking certificate past it)."
else
  echo "FAILED"; exit 1
fi
