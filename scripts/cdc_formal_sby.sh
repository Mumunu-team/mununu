#!/usr/bin/env bash
# Functional multi-clock check with SymbiYosys (`multiclock on`), outside mununu's engines.
#
# mununu's lift has one implicit clock and refuses a multi-clock design; `mununu sv cdc-lint`
# classifies the crossings structurally. What neither answers is a functional question across
# clocks, for example whether a one-cycle pulse always reaches the other domain. SymbiYosys does:
# with `multiclock on`, yosys `clk2fflogic` turns every clock into a free input sampled on a
# global step, so the engines explore every interleaving of the clock edges (ratios included,
# unless an `assume` constrains them). Metastability is NOT modelled: a flop sampling a changing
# input takes the old value, so incoherent multi-bit samples are the lint's question, not this one.
#
# Usage:
#   scripts/cdc_formal_sby.sh --top TOP [--mode prove|bmc|cover] [--depth N]
#                             [--engine 'abc pdr' | 'smtbmc bitwuzla' | ...] [--slang]
#                             [--define NAME[=VALUE]]... [--outdir DIR] FILE.sv...
#
# The design's assertions are read with `read -formal`, so `ifdef FORMAL` blocks,
# `always @($global_clock)` and immediate `assert` / `assume` statements work; `--slang` reads
# with the yosys-slang plugin instead (SystemVerilog concurrent assertions). Runs `sby` directly
# when it is on PATH, else inside the `mununu-sva` image. Exit status: 0 PASS, 2 FAIL, 1 other.
set -euo pipefail

top="" mode="prove" depth=40 engine="" slang=0 outdir="cdc_sby" defines=() files=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --top) top="$2"; shift 2 ;;
    --mode) mode="$2"; shift 2 ;;
    --depth) depth="$2"; shift 2 ;;
    --engine) engine="$2"; shift 2 ;;
    --slang) slang=1; shift ;;
    --define) defines+=("$2"); shift 2 ;;
    --outdir) outdir="$2"; shift 2 ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 1 ;;
    *) files+=("$1"); shift ;;
  esac
done
[[ -n "$top" && ${#files[@]} -gt 0 ]] || { echo "usage: $0 --top TOP FILE.sv..." >&2; exit 1; }
[[ "$top" =~ ^[A-Za-z_][A-Za-z0-9_$]*$ ]] || { echo "invalid --top: $top" >&2; exit 1; }
case "$mode" in prove|bmc|cover) ;; *) echo "--mode must be prove, bmc or cover" >&2; exit 1 ;; esac
[[ "$depth" =~ ^[0-9]+$ ]] || { echo "--depth must be a number" >&2; exit 1; }
if [[ -z "$engine" ]]; then
  [[ "$mode" == prove ]] && engine="abc pdr" || engine="smtbmc bitwuzla"
fi
[[ "$engine" =~ ^[A-Za-z0-9_\ .-]+$ ]] || { echo "invalid --engine: $engine" >&2; exit 1; }

mkdir -p "$outdir"
names=() defs=""
for f in "${files[@]}"; do
  [[ -f "$f" ]] || { echo "no such file: $f" >&2; exit 1; }
  cp "$f" "$outdir/"
  names+=("$(basename "$f")")
done
for d in "${defines[@]:-}"; do
  [[ -z "$d" ]] && continue
  [[ "$d" =~ ^[A-Za-z_][A-Za-z0-9_]*(=[A-Za-z0-9_\']*)?$ ]] || { echo "invalid --define: $d" >&2; exit 1; }
  defs+=" -D$d"
done
if [[ $slang -eq 1 ]]; then
  read_cmd="plugin -i slang
read_slang${defs} --top $top ${names[*]}"
else
  read_cmd="read -formal${defs} ${names[*]}"
fi

cat > "$outdir/cdc.sby" <<EOF
[options]
mode $mode
depth $depth
multiclock on

[engines]
$engine

[script]
$read_cmd
prep -top $top

[files]
$(printf '%s\n' "${names[@]}")
EOF

run_sby() { (cd "$outdir" && sby -f cdc.sby); }
if command -v sby >/dev/null 2>&1; then
  log=$(run_sby 2>&1) || true
else
  abs=$(cd "$outdir" && pwd)
  log=$(docker run --rm -v "$abs":/work -w /work mununu-sva:latest sby -f cdc.sby 2>&1) || true
fi
printf '%s\n' "$log" | grep -E "summary:|DONE|Assert failed|trace|Status:" || printf '%s\n' "$log" | tail -20

status=$(printf '%s\n' "$log" | sed -n 's/.*DONE (\([A-Z]*\).*/\1/p' | tail -1)
case "$status" in
  PASS) echo "cdc_formal_sby: PASS ($mode, depth $depth, engine '$engine')"; exit 0 ;;
  FAIL) echo "cdc_formal_sby: FAIL — trace under $outdir/cdc/engine_0/"; exit 2 ;;
  *) echo "cdc_formal_sby: ${status:-ERROR}"; exit 1 ;;
esac
