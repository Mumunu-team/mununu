#!/usr/bin/env bash
# HWMCC'17 LIVE study — `mununu btor2 verify-justice` over the competition's AIGER-1.9 justice
# benchmarks, adjudicated against the published per-instance results (`live.csv`: abclive, aigbmc,
# iimc, nuxmvlive; sat = a fair cycle exists = our VIOLATED, uns = our HOLDS).
#
# The benchmark set is NOT vendored (same posture as the HWMCC'20 bv study): download
# https://fmv.jku.at/hwmcc17/hwmcc17-live-benchmarks.tar.xz and live.csv into a directory and point
# MUNUNU_HWMCC_LIVE_DIR at it. Resumable: an instance with a row in the results file is skipped.
#
#   MUNUNU_HWMCC_LIVE_DIR=~/hwmcc17-live MAX_LATCHES=192 BUDGET=600 scripts/hwmcc_live_study.sh
#
# Columns of the results file (TSV): name latches inputs ands rung verdict decided_by elapsed_ms
# ground_truth agreement. `agreement` is `ok`, `DISAGREE` (a soundness flag — stop and look), or
# `-` when no published solver decided the instance.
set -uo pipefail
DIR=${MUNUNU_HWMCC_LIVE_DIR:-$HOME/hwmcc17-live}
BIN=${MUNUNU_BIN:-mununu}
MAX_LATCHES=${MAX_LATCHES:-192}
MIN_LATCHES=${MIN_LATCHES:-0}
BUDGET=${BUDGET:-600}          # seconds per instance (wall)
RUNG=${RUNG:-both}
OUT=${OUT:-$DIR/results-mununu-$RUNG${NSHARDS:+-shard${SHARD:-0}}.tsv}
CSV=$DIR/live.csv
[ -f "$CSV" ] || { echo "no $CSV"; exit 2; }
touch "$OUT"
truth() { # name -> sat|uns|-
  awk -F';' -v n="$1" 'NR>1 && $1==n { for (i=3;i<=NF;i+=6) if ($i=="sat"||$i=="uns") { print $i; exit } print "-" }' "$CSV"
}
# Sharding for parallel runs: NSHARDS workers each take every NSHARDS-th instance (SHARD = 0..NSHARDS-1).
NSHARDS=${NSHARDS:-1}; SHARD=${SHARD:-0}; idx=-1
for f in "$DIR"/*.aig; do
  idx=$((idx+1)); [ $((idx % NSHARDS)) -eq "$SHARD" ] || continue
  name=$(basename "$f" .aig)
  grep -q "^$name	" "$OUT" && continue
  read -r _ _M I L _O A _rest < <(head -c 200 "$f" | head -1)
  [ "$L" -ge "$MIN_LATCHES" ] && [ "$L" -le "$MAX_LATCHES" ] || continue
  t0=$(date +%s%3N 2>/dev/null || python3 -c 'import time;print(int(time.time()*1000))')
  # shellcheck disable=SC2086
  json=$(timeout "$BUDGET" "$BIN" btor2 verify-justice "$f" --rung "$RUNG" --json ${EXTRA_ARGS:-} 2>/dev/null); rc=$?
  t1=$(date +%s%3N 2>/dev/null || python3 -c 'import time;print(int(time.time()*1000))')
  if [ $rc -eq 124 ]; then verdict=timeout; by=-; ms=$((BUDGET*1000))
  elif [ -z "$json" ]; then verdict=error; by=-; ms=$((t1-t0))
  else
    verdict=$(printf '%s' "$json" | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["verdict"])' 2>/dev/null || echo error)
    by=$(printf '%s' "$json" | python3 -c 'import json,sys; j=json.load(sys.stdin); print("+".join(j["justice"][0]["decided_by"]) or "-")' 2>/dev/null || echo -)
    ms=$(printf '%s' "$json" | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["elapsed_ms"])' 2>/dev/null || echo $((t1-t0)))
  fi
  gt=$(truth "$name")
  case "$gt:$verdict" in
    sat:violated|uns:holds) agree=ok ;;
    sat:holds|uns:violated) agree=DISAGREE ;;
    *) agree=- ;;
  esac
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$L" "$I" "$A" "$RUNG" "$verdict" "$by" "$ms" "$gt" "$agree" | tee -a "$OUT"
done
echo "== summary ($OUT)"
awk -F'\t' 'NR>0 { n++; v[$6]++; if ($10=="ok") ok++; if ($10=="DISAGREE") bad++; if ($9!="-") gt++ } END { printf "instances %d · holds %d · violated %d · unknown %d · timeout %d · error %d · agree %d/%d · DISAGREE %d\n", n, v["holds"], v["violated"], v["unknown"], v["timeout"], v["error"], ok, gt, bad }' "$OUT"
