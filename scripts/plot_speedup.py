#!/usr/bin/env python3
"""
plot_speedup.py — emit blog/paper figures from a Criterion archive.

Usage:
  scripts/plot_speedup.py --exp EXP-NNNN-slug --kind speedup
  scripts/plot_speedup.py --exp EXP-NNNN-slug --kind memory
  scripts/plot_speedup.py --exp EXP-NNNN-slug --kind scaling

Reads experiments/<EXP>/criterion-archive.tar.zst and emits an SVG and PNG
to experiments/<EXP>/figures/<kind>.svg|png (override with --out-dir); the
write-up that embeds them is kept with the private publication drafts.

Stub for now: prints what it WOULD do. The real plotting waits until at
least one archived EXP exists to drive the layout decisions.
"""
import argparse
import json
import sys
import tarfile
from pathlib import Path


def cmd_speedup(exp_dir: Path, out_dir: Path) -> int:
    archive = exp_dir / "criterion-archive.tar.zst"
    if not archive.exists():
        print(f"error: {archive} not found", file=sys.stderr)
        return 1
    print(f"[stub] would extract {archive} and plot median + 95% CI per bench")
    print(f"[stub] would write {out_dir}/speedup.svg and {out_dir}/speedup.png")
    return 0


def cmd_memory(exp_dir: Path, out_dir: Path) -> int:
    archive = exp_dir / "dhat-archive.tar.zst"
    if not archive.exists():
        print(f"warning: {archive} not found; memory plot needs a dhat archive", file=sys.stderr)
        return 1
    print(f"[stub] would extract {archive} and plot peak/allocs/copies")
    print(f"[stub] would write {out_dir}/memory.svg and {out_dir}/memory.png")
    return 0


def cmd_scaling(exp_dir: Path, out_dir: Path) -> int:
    print("[stub] scaling plot reads the criterion archive across "
          "RAYON_NUM_THREADS values from the bench id")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--exp", required=True, help="experiment ID, e.g. EXP-0002-iter-rank-soa")
    ap.add_argument("--kind", required=True, choices=("speedup", "memory", "scaling"))
    ap.add_argument("--out-dir", default=None, help="figure output directory")
    args = ap.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    exp_dir = repo_root / "experiments" / args.exp
    if not exp_dir.is_dir():
        print(f"error: {exp_dir} not found", file=sys.stderr)
        return 2
    out_dir = Path(args.out_dir) if args.out_dir else (
        exp_dir / "figures"
    )
    out_dir.mkdir(parents=True, exist_ok=True)

    if args.kind == "speedup":
        return cmd_speedup(exp_dir, out_dir)
    if args.kind == "memory":
        return cmd_memory(exp_dir, out_dir)
    if args.kind == "scaling":
        return cmd_scaling(exp_dir, out_dir)
    return 2


if __name__ == "__main__":
    sys.exit(main())
