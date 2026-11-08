#!/usr/bin/env bash
# mununu#545 / roadmap F1 — the capture-timing fixture, both variants, with the expected verdicts
# asserted by the tool itself (`--expect`). Run inside the mununu-sva image (slang is the front
# end); `mununu` must be on PATH or named by MUNUNU.
#
# Exit 0 = the design holds both properties AND the twin violates both (the fixture answers
# "routing": the array-aware engines decide the shape as written). Any other outcome is the
# measurement #545 was closed waiting for: a ⊥ on either variant means the shape needs a
# value-only havoc at the read (design work), and is reported by the tool with its reason.
set -euo pipefail
cd "$(dirname "$0")"
M="${MUNUNU:-mununu}"

echo "== line_table (expect: both hold)"
"$M" --quiet sv verify-auto line_table.sv --source line_table_sva.sv --source line_table_bind.sv --top line_table \
  --expect a_enable_holds_between_strobes=holds \
  --expect a_disable_holds_between_strobes=holds \
  --json > logs/line_table.json
echo "== line_table_twin (expect: both violated)"
"$M" --quiet sv verify-auto line_table_twin.sv --source line_table_sva.sv --source line_table_twin_bind.sv \
  --top line_table_twin \
  --expect a_enable_holds_between_strobes=violated \
  --expect a_disable_holds_between_strobes=violated \
  --json > logs/line_table_twin.json
echo "F1 fixture: design holds, twin violated — the shape is decided as written (routing, not design work)"
