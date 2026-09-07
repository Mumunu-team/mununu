#!/usr/bin/env bash
# mununu#504 (point 2) — run the `#[ignore]`d e2e suite ONE TEST PER PROCESS.
#
# WHY THIS EXISTS
#
# `make e2e` runs the whole suite inside a single libtest process. A test that
# ABORTS rather than fails takes that process with it: a stack overflow calls
# `rtabort!` and a panic while unwinding an exhausted BDD arena calls `abort()`
# — neither unwinds, so neither is a catchable test failure. When that happens
# libtest prints NO summary line and NO failure list, the run reads like a build
# error, and every test scheduled after the aborting one silently never runs.
#
# That is not hypothetical: it is how the ignored set drifted to 19 unnoticed
# failures (mununu#503), found while triaging mununu#504.
#
# Running each test in its own process turns an abort into ONE reported CRASH
# row and lets the rest of the suite finish, so the sweep always produces a
# complete picture of what passed, what failed, and what died.
#
# USAGE (inside the pinned image — see CLAUDE.md §"SVA-verification e2e validation")
#
#   docker run --rm -v "$(pwd)":/work -v mununu-target:/ct -w /work \
#     -e CARGO_TARGET_DIR=/ct mununu-sva bash -c \
#     'export PATH=$HOME/.cargo/bin:/opt/oss-cad-suite/bin:$PATH; ./scripts/e2e-sweep.sh'
#
#   FILTER=e2e_sysrst ./scripts/e2e-sweep.sh   # restrict to matching test names
set -uo pipefail

CARGO="${CARGO:-cargo}"
FILTER="${FILTER:-e2e_}"

echo "== building e2e test binaries =="
lib_bin=$($CARGO test -p mununu-core --lib --all-features --no-run --message-format=json 2>/dev/null \
  | grep -o '"executable":"[^"]*mununu_core[^"]*"' | tail -1 | cut -d'"' -f4)
oracle_bin=$($CARGO test -p mununu-core --test differential_oracle_e2e --all-features --no-run --message-format=json 2>/dev/null \
  | grep -o '"executable":"[^"]*differential_oracle_e2e[^"]*"' | tail -1 | cut -d'"' -f4)

if [ -z "${lib_bin:-}" ]; then
  echo "FATAL: could not locate the mununu-core lib test binary — build failed?" >&2
  exit 2
fi

pass=0; fail=0; crash=0
declare -a failed_names=() crashed_names=()

run_binary() {
  local bin="$1" label="$2"
  [ -n "$bin" ] && [ -x "$bin" ] || { echo "(skipping $label — not built)"; return; }
  echo
  echo "== $label: $bin =="
  # `--list` with `--ignored` enumerates exactly the ignored tests.
  local names
  names=$("$bin" --ignored --list --format terse 2>/dev/null | sed 's/: test$//' | grep -- "$FILTER" || true)
  [ -n "$names" ] || { echo "(no tests matching '$FILTER')"; return; }

  while IFS= read -r name; do
    [ -n "$name" ] || continue
    local t0 rc dt
    t0=$(date +%s)
    "$bin" --ignored --exact "$name" --nocapture > "/tmp/e2e-$$.log" 2>&1
    rc=$?
    dt=$(( $(date +%s) - t0 ))
    case $rc in
      0)   printf '  %-6s %4ds  %s\n' "PASS" "$dt" "$name"; pass=$((pass+1)) ;;
      101) printf '  %-6s %4ds  %s\n' "FAIL" "$dt" "$name"; fail=$((fail+1)); failed_names+=("$name")
           sed -n '/panicked at/,+3p' "/tmp/e2e-$$.log" | head -6 | sed 's/^/          /' ;;
      *)   # 134 = SIGABRT (stack overflow / abort-on-unwind), 139 = SIGSEGV, ...
           # Name the cause when the log says it — a stack overflow and an
           # abort-while-unwinding both surface as SIGABRT but need different
           # fixes, and telling them apart is the whole point of mununu#504.
           local cause=""
           grep -q "has overflowed its stack" "/tmp/e2e-$$.log" && cause=" — STACK OVERFLOW"
           grep -q "memory allocation of" "/tmp/e2e-$$.log" && cause=" — allocator OOM"
           printf '  %-6s %4ds  %s  (exit %d%s)%s\n' "CRASH" "$dt" "$name" "$rc" \
             "$([ $rc -gt 128 ] && echo ", signal $((rc-128))")" "$cause"
           crash=$((crash+1)); crashed_names+=("$name")
           tail -8 "/tmp/e2e-$$.log" | sed 's/^/          /' ;;
    esac
    rm -f "/tmp/e2e-$$.log"
  done <<< "$names"
}

run_binary "$lib_bin" "mununu-core lib e2e"
run_binary "${oracle_bin:-}" "differential_oracle_e2e"

echo
echo "=========================================================="
printf 'e2e sweep: %d passed, %d failed, %d CRASHED\n' "$pass" "$fail" "$crash"
[ ${#failed_names[@]}  -gt 0 ] && printf '  failed:  %s\n' "${failed_names[@]}"
[ ${#crashed_names[@]} -gt 0 ] && printf '  CRASHED: %s\n' "${crashed_names[@]}"
echo "=========================================================="
# A crash is worse than a failure: it means a test killed its process rather
# than reporting a verdict. Surface both, but never swallow either.
[ $((fail + crash)) -eq 0 ]
