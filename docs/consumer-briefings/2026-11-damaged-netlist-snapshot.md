# Consumer briefing: a damaged netlist snapshot is a lift error or a cache miss, not a design with no clock (mununu#1198, 2026-11)

> Source of truth: [`snapshot_modules`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and [`clock_domains_with_hierarchy`](../../crates/mununu-core/src/adapter/yosys/mod.rs), and `LiftCache::lookup` in [`adapter/yosys/lift_cache.rs`](../../crates/mununu-core/src/adapter/yosys/lift_cache.rs) — surface: CLI+API — every `sv` verb and route that lifts SystemVerilog

**Audience:** anyone who lifts SystemVerilog with `MUNUNU_LIFT_CACHE_DIR` set (rosf and monono CI, `make e2e`), and anyone who matches on lift error text. It follows [the briefing of mununu#956 / #957](2026-11-lift-cache-key-and-snapshots.md), which made a *missing* or *empty* snapshot a lift error.

## TL;DR

- **A bugfix on one check, the multi-clock refusal.** The lift counts the clock domains on a JSON netlist its own yosys script writes (`clocks.json`), and reads the stubs' port directions from a second one (`hier.json`). When the netlist was not a netlist — a truncated file, text that is not JSON, JSON with no module in it — the count came back empty, which is also what a design with no storage cell gives. So nothing was refused, and under `--merge-clock-domains` nothing was noted: a two-clock design lifted onto one clock as if it had one.
- **Where it could happen:** a yosys run that exits 0 after writing a partial snapshot (the lift went through, and with the cache on its outputs were stored and served again on every hit), and a cache entry whose stored snapshot text is damaged while the entry file itself still parses (a cache hit read the stored snapshots with no check; an entry file that does not parse was already a miss).
- **Now, on a cache hit:** an entry whose `hier.json` or `clocks.json` is not a netlist is a **cache miss**. It is logged at `warn` (`lift cache: the entry's flattened netlist snapshot cannot be used (it is not JSON: …); treating as a miss`), the design is lifted again, and that lift replaces the entry.
- **Now, on a fresh lift:** the lift fails with `adapter/yosys: yosys ran but its flattened netlist snapshot <path> cannot be used (<what is wrong>); … — a mununu defect, not something the input can change: please report it …`. `<what is wrong>` is one of `it is empty`, `it is not JSON: <the parser's message>`, `it holds no module`, ``its module `<name>` has no list of cells``. The same check runs on the hierarchy snapshot. On the per-module lift (`sv emit-btor2-per-module`, `verify project` with `multi_module = true`) a damaged discovery snapshot was already an error, worded as if the design had no module (`found no non-blackbox modules in the hierarchy snapshot`); it now names the snapshot and the damage.
- **No JSON field is added, renamed or removed, and no verdict value changes on a lift whose snapshots are intact.** A lift that went through on a damaged snapshot now fails or runs again.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | With `MUNUNU_LIFT_CACHE_DIR`, a damaged entry costs one fresh lift instead of yielding a model whose clock check did not run. Without the cache, only a yosys that exits 0 with a partial snapshot changes outcome: from a lift to an error. | None. |
| **monono** | Same. | Same. |
| **mununu-ui** | Nothing: no route, field or note changes. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the check. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

No cache directory needs clearing: the cache format is unchanged, an intact entry is still a hit, and an entry stored by an earlier build with a damaged snapshot is replaced by the lift that finds it.

## Provenance

- **Found by:** the case table of mununu#1180's fix, which listed "a netlist snapshot that does not parse" as open (mununu#1198). The defect was read off the code, not met on a real cache directory: no damaged entry was observed in the field.
- **Tests:** `x1198_a_cache_hit_with_a_damaged_netlist_is_not_a_lift_with_no_clock` (the reproducer: a two-clock netlist truncated in the cache lifts on the parent and is a lift error here), `x1198_the_clock_scan_tells_a_damaged_snapshot_from_a_design_with_no_storage_cell`, `x1198_the_lift_refuses_a_snapshot_that_is_not_a_netlist`, `x1198_an_entry_with_a_damaged_snapshot_is_a_miss_and_is_replaced`; the real lift's snapshots pass the check in the image test `e2e_lift_cache_second_lift_is_a_hit_with_identical_artifacts`.
- **Class:** `clock-net-driver` in [`docs/defect-classes.md`](../defect-classes.md).

## Not covered here

- A damaged model text (`design.btor`) in a cache entry: not checked by this change. The entries carry no checksum.
- A snapshot that is still a netlist after damage (a valid JSON file from another design). The cache key names the entry, not its content.
- The time the extra parse of the two snapshots adds to a cache hit: not measured.
- The readers of the two snapshots other than the clock scan still answer "nothing found" for a text they cannot parse. None is reached with one now, since both snapshots are checked on the way in; they were not changed.
