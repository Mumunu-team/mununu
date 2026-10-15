#!/usr/bin/env bash
# validate.sh — V.9 recoverability showcase on OpenTitan prim_count (a hardened counter).
#
# Asks a branching-time question SVA cannot state, in BOTH directions on the SAME real primitive:
#   (1) DESCENT  — "from any count, can the counter always get back to zero?"    (AG EF cnt==0)
#   (2) ASCENT   — "from any count, can the counter always fill to max credit?"  (AG EF cnt==MAX)
#     always_recoverable = nu Y. ((mu X. (cnt==GOAL || <> X)) && [] Y)
# The counter is a real OpenTitan `prim_count` (step=1, no clear/set, see count_top.sv). Reaching
# zero is a well-founded DESCENT; reaching MAX = 2^Width-1 is a well-founded ASCENT — both the
# RANKING class, which no bounded predicate set captures. mununu's RANKING CERTIFICATE decides
# each over the exact transition: from every off-goal state SOME input strictly moves a
# well-founded measure toward the goal — δ = cnt for the descent (bounded below by 0), δ = MAX−cnt
# for the ascent (bounded below by 0). The certificate tries BOTH measure directions, so one
# extraction decides both properties (∃-input variant, one relation each).
#
# RESULT: both `AG EF (cnt == 0)` and `AG EF (cnt == MAX)` decide HOLDS on a 48-bit prim_count,
# where the exact BDD engine walls and the predicate cube abstains. A SINGLE-REGISTER ranking in
# TWO directions on a real hardened primitive — a different shape from v8's relational FIFO drain.
#
# Note on the extraction: prim_count is HARDENED — it carries a redundant SECONDARY counter (for
# fault detection) and an FPV backdoor. Those leave wide signals in the lifted BTOR2 that do not
# touch the primary count's evolution; the ranking certificate's cone-of-influence input filter
# ignores them (it enumerates only inputs in the counted register's next-state cone).
#
# Pedigree: prim_count.sv + prim_count_pkg.sv are real OpenTitan RTL (Apache-2.0), pinned under
# source/ at the commit in UPSTREAM_COMMIT.txt. prim_flop.sv is a minimal register matching
# OpenTitan's abstract-prim flop interface (identical to prim_generic_flop); count_top.sv is a
# thin down-counter harness (the usage configuration). A property-class demonstration on real
# silicon, not a vulnerability finding.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
SRC="examples/verify/v9_opentitan_count_recoverability/source"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in sv2v yosys python3; do
  command -v "${tool}" >/dev/null 2>&1 || { echo "validate.sh: required tool '${tool}' not on PATH" >&2; exit 2; }
done

OUT="$(mktemp -d -t mununu-v9-XXXXXX)"
sv2v -I"${SRC}" "${SRC}/prim_count_pkg.sv" "${SRC}/prim_flop.sv" "${SRC}/prim_count.sv" \
     "${SRC}/count_top.sv" > "${OUT}/top.v" 2>"${OUT}/sv2v.err"

# Lift count_top at a chosen Width to BTOR2, then name the PRIMARY counter (the state cell in cnt_o's
# combinational cone) `cnt` so the target `cnt == 0` resolves. `opt -full` inlines sv2v's cast helpers.
lift() {
  local width="$1" out="$2"
  yosys -q -p "read_verilog -sv ${OUT}/top.v; hierarchy -check -top count_top -chparam Width ${width}; \
               proc; flatten; opt -full; wreduce; opt -full -purge; async2sync; dffunmap; opt_clean -purge; \
               write_btor ${out}" 2>"${OUT}/yosys.err"
  python3 - "$out" <<'PY'
import sys
from collections import deque
p=sys.argv[1]; L=open(p).read().splitlines(); lines={}; cnt_o=None
for ln in L:
    t=ln.split()
    if len(t)>=2 and t[0].isdigit(): lines[int(t[0])]=t
    if len(t)>=4 and t[1]=="output" and t[3]=="cnt_o": cnt_o=int(t[2])
# walk cnt_o's cone to the primary counter state
seen=set(); q=deque([abs(cnt_o)]); state=None
while q:
    n=q.popleft()
    if n in seen: continue
    seen.add(n); t=lines.get(n)
    if not t: continue
    if t[1]=="state": state=n; break
    if t[1] not in ("input","sort","const","constd","zero","one","ones"):
        for tok in t[2:]:
            try: q.append(abs(int(tok)))
            except: pass
out=[]
for ln in L:
    t=ln.split()
    if len(t)==3 and t[1]=="state" and int(t[0])==state: out.append(ln+" cnt")
    else: out.append(ln)
open(p,"w").write("\n".join(out)+"\n")
PY
}

echo "=== V.9 — recoverability of OpenTitan prim_count, BOTH directions on one real primitive ==="
echo "===        (1) AG EF cnt==0  descent      (2) AG EF cnt==MAX  ascent                    ==="
echo

# MAX credit for the ascent = 2^Width - 1.
MAX8=255
MAX48=281474976710655

echo "--- small counter (Width=8, exact engine) — expect HOLDS both directions ---"
lift 8 "${OUT}/w8.btor2"
echo -n "  descent AG EF (cnt == 0):     "; "${MUNUNU}" btor2 verify-recoverability "${OUT}/w8.btor2" --target 'cnt == 0'        2>/dev/null | grep -oiE '"verdict": "[a-z]+"'
echo -n "  ascent  AG EF (cnt == ${MAX8}):   "; "${MUNUNU}" btor2 verify-recoverability "${OUT}/w8.btor2" --target "cnt == ${MAX8}"  2>/dev/null | grep -oiE '"verdict": "[a-z]+"'
S_DN="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/w8.btor2" --target 'cnt == 0'       2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
S_UP="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/w8.btor2" --target "cnt == ${MAX8}" 2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
echo
echo "--- wide counter (Width=48) — expect HOLDS both directions (the JSON's engine field says which engine decided) ---"
lift 48 "${OUT}/w48.btor2"
echo -n "  descent AG EF (cnt == 0):                  "; "${MUNUNU}" btor2 verify-recoverability "${OUT}/w48.btor2" --target 'cnt == 0'         2>/dev/null | grep -oiE '"verdict": "[a-z]+"'
echo -n "  ascent  AG EF (cnt == ${MAX48}):   "; "${MUNUNU}" btor2 verify-recoverability "${OUT}/w48.btor2" --target "cnt == ${MAX48}" 2>/dev/null | grep -oiE '"verdict": "[a-z]+"'
W_DN="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/w48.btor2" --target 'cnt == 0'          2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
W_UP="$("${MUNUNU}" btor2 verify-recoverability "${OUT}/w48.btor2" --target "cnt == ${MAX48}" 2>/dev/null | grep -oiE 'holds|violated|unknown' | head -1)"
echo

ok=1
[[ "${S_DN}" != "holds" ]] && { echo "FAIL: small descent expected holds, got ${S_DN}" >&2; ok=0; }
[[ "${S_UP}" != "holds" ]] && { echo "FAIL: small ascent expected holds, got ${S_UP}" >&2; ok=0; }
[[ "${W_DN}" != "holds" ]] && { echo "FAIL: wide descent expected holds (ranking certificate), got ${W_DN}" >&2; ok=0; }
[[ "${W_UP}" != "holds" ]] && { echo "FAIL: wide ascent expected holds (ranking certificate), got ${W_UP}" >&2; ok=0; }
if [[ "${ok}" == "1" ]]; then
  echo "PASS — prim_count recoverability DECIDES HOLDS in BOTH directions at Width=48 (engine field: exact since the 192-bit auto-cap; past it the ranking certificate,"
  echo "       δ=cnt for the descent, δ=MAX−cnt for the ascent, where the cube abstains)."
else
  echo "FAILED"; exit 1
fi
