#!/usr/bin/env bash
# validate.sh — V.10 recoverability showcase: a NESTED counter built from real OpenTitan prim_count.
#
# The prescaler+counter pattern: an inner prim_count down-counts each tick and reloads to INNER_MAX at
# zero; when it reloads, an outer prim_count decrements. Asks whether the outer counter can always get
# back to zero — recoverability over a NESTED descent:
#     always_recoverable = nu Y. ((mu X. (outer==0 || <> X)) && [] Y)   (AG EF outer==0)
#
# `outer` ALONE is not a ranking (it holds while `inner` runs down), so a single-register ranking
# certificate fails. mununu's ranking certificate generalizes to LEXICOGRAPHIC measures: the tuple
# (outer, inner) decreases every tick (inner down with outer fixed, or outer down when inner reloads),
# and lex order on the tuple is well-founded — so `AG EF (outer == 0)` decides HOLDS on a 48-bit outer
# counter, where a single-register ranking, the predicate cube, and the exact BDD engine all give out.
#
# Pedigree: prim_count.sv + prim_count_pkg.sv are real OpenTitan RTL (Apache-2.0), pinned under source/
# at the commit in UPSTREAM_COMMIT.txt. prim_flop.sv is a minimal register matching OpenTitan's
# abstract-prim flop interface; count_nested_top.sv is a nested-counter harness of two real prim_count
# instances (the prescaler+counter usage). A property-class demonstration on real silicon.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
SRC="examples/verify/v10_opentitan_nested_count_recoverability/source"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in sv2v yosys python3; do
  command -v "${tool}" >/dev/null 2>&1 || { echo "validate.sh: required tool '${tool}' not on PATH" >&2; exit 2; }
done

OUT="$(mktemp -d -t mununu-v10-XXXXXX)"
sv2v -I"${SRC}" "${SRC}/prim_count_pkg.sv" "${SRC}/prim_flop.sv" "${SRC}/prim_count.sv" \
     "${SRC}/count_nested_top.sv" > "${OUT}/top.v" 2>"${OUT}/sv2v.err"

# Lift, then name the OUTER counter (the state cell in outer_o's cone) `outer` so the target resolves.
lift() {
  local ow="$1" iw="$2" imax="$3" oinit="$4" out="$5"
  yosys -q -p "read_verilog -sv ${OUT}/top.v; hierarchy -check -top count_nested_top \
                 -chparam OW ${ow} -chparam IW ${iw} -chparam INNER_MAX ${iw}'d${imax} -chparam OUTER_INIT ${ow}'d${oinit}; \
               proc; flatten; opt -full; wreduce; opt -full -purge; async2sync; dffunmap; opt_clean -purge; \
               write_btor ${out}" 2>"${OUT}/yosys.err"
  python3 - "$out" <<'PY'
import sys
from collections import deque
p=sys.argv[1]; L=open(p).read().splitlines(); lines={}; oo=None
for ln in L:
    t=ln.split()
    if len(t)>=2 and t[0].isdigit(): lines[int(t[0])]=t
    if len(t)>=4 and t[1]=="output" and t[3]=="outer_o": oo=int(t[2])
seen=set(); q=deque([abs(oo)]); st=None
while q:
    n=q.popleft()
    if n in seen: continue
    seen.add(n); t=lines.get(n)
    if not t: continue
    if t[1]=="state": st=n; break
    if t[1] not in ("input","sort","const","constd","zero","one","ones"):
        for tok in t[2:]:
            try: q.append(abs(int(tok)))
            except: pass
out=[]
for ln in L:
    t=ln.split()
    if len(t)==3 and t[1]=="state" and int(t[0])==st: out.append(ln+" outer")
    else: out.append(ln)
open(p,"w").write("\n".join(out)+"\n")
PY
}

echo "=== V.10 — nested-counter recoverability on real OpenTitan prim_count (AG EF outer==0) ==="
echo
echo "--- small (OW=8, exact engine) — expect HOLDS ---"
lift 8 4 6 40 "${OUT}/small.btor2"
"${MUNUNU}" btor2 verify-recoverability "${OUT}/small.btor2" --target 'outer == 0' 2>/dev/null | grep -iE '"verdict"'
SMALL="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/small.btor2" --target 'outer == 0' 2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
echo
echo "--- wide (OW=48) — expect HOLDS (the JSON's engine field says which engine decided) ---"
lift 48 8 100 1099511627776 "${OUT}/wide.btor2"
"${MUNUNU}" btor2 verify-recoverability "${OUT}/wide.btor2" --target 'outer == 0' 2>/dev/null | grep -iE '"verdict"'
WIDE="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/wide.btor2" --target 'outer == 0' 2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
echo

ok=1
[[ "${SMALL}" != "holds" ]] && { echo "FAIL: small expected holds, got ${SMALL}" >&2; ok=0; }
[[ "${WIDE}"  != "holds" ]] && { echo "FAIL: wide expected holds (lexicographic ranking), got ${WIDE}" >&2; ok=0; }
if [[ "${ok}" == "1" ]]; then
  echo "PASS — nested-counter recoverability DECIDES HOLDS on real prim_count at OW=48 (engine field: exact since the 192-bit auto-cap; past it the LEXICOGRAPHIC ranking, where a single-register ranking and the cube give out)."
else
  echo "FAILED"; exit 1
fi
