#!/usr/bin/env python3
"""Vendor one OpenTitan IP with its source closure for `sv verify-auto` — no fusesoc needed.

    scripts/vendor_opentitan_ip.py --ip edn --commit 558921ccc8aeefab652b45c1402c10bceb63accc \
        --checkout /tmp/ot --out examples/verify/ip_opentitan_edn/source \
        --rewrite "edn_core.sv|'{default: 1'b0}|'{default: '0}"

Run inside the `mununu-sva` image (it needs `slang`; `git` and Python 3 only otherwise). What it does:

1. A sparse checkout of lowRISC/opentitan at the commit (`hw/ip`, `hw/top_earlgrey/{ip_autogen,rtl}`),
   unless `--checkout` already holds one.
2. The closure: starting from the IP's `rtl/` directory, run slang on the growing file set and add,
   for every `unknown module` / `unknown package` / missing-include diagnostic, the file defining
   the name over the allowed RTL directories (`hw/ip/prim_generic/rtl` preferred for `prim_*`
   implementations), then every `include`d file, until slang is clean. `--blackbox NAME` leaves a
   module out and writes `NAME_bbox.sv`: its upstream port list verbatim under `(* blackbox *)`,
   no body — the lift frees its outputs (a declared over-approximation; the stub's own package
   dependencies are resolved like any other).
3. `--rewrite FILE|FROM|TO` applies a documented one-line local change (sv2v leaves the sized
   `'{default: 1'b0}` on an unpacked-array port connection and yosys's Verilog frontend rejects it;
   the unsized `'{default: '0}` is the same value and both frontends accept it). Every rewrite is
   recorded.
4. The synthesis shim `prim_assert_shim.sv`: an EMPTY definition for every macro the real
   `prim_assert` set declares (so the lift compiles the assertions out, as OpenTitan's own
   `prim_assert_dummy_macros.svh` does), with `ASSERT_DEFAULT_CLK/RST`, `_SEC_CM_ALERT_MAX_CYC`
   and the `prim_flop_macros.sv` include kept so `PRIM_FLOP_SPARSE_FSM` still instantiates
   `prim_sparse_fsm_flop` (its `unused_err_o` is what the FSM error → alert checks read).
   Extraction reads the real macros; only the lift sees the shim (`--lift-override`).
5. `sources.txt` (compile order: packages by dependency, the macro set, `.svh`, modules, primary
   last), `UPSTREAM_COMMIT.txt` (commit + `<file>  <upstream path>` per source + the local stubs
   and rewrites), and `run.sh` — the exact `sv verify-auto` command.

Standard library only.
"""
import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

DEF_RE = re.compile(r"^\s*(module|package|interface)\s+([A-Za-z_]\w*)", re.M)
UNKNOWN_RE = re.compile(
    r"unknown (?:module|class or package|package|interface|interface or program)\s+'([^']+)'"
)
INCLUDE_RE = re.compile(
    r"'([^']+\.svh?)' file not found|could not find include file '([^']+)'|no such file or directory: '([^']+)'"
)
PKG_REF_RE = re.compile(r"\b([A-Za-z_]\w*)::")
IMPORT_RE = re.compile(r"\bimport\s+([A-Za-z_]\w*)::")
INC_RE = re.compile(r'`include\s+"([^"]+)"')
MACRO_SET = (
    "prim_assert.sv",
    "prim_assert_standard_macros.svh",
    "prim_assert_sec_cm.svh",
    "prim_flop_macros.sv",
)
SPARSE = ["hw/ip", "hw/top_earlgrey/ip_autogen", "hw/top_earlgrey/rtl"]


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def sparse_checkout(checkout: Path, commit: str):
    if (checkout / "hw/ip").is_dir():
        head = sh(["git", "-C", str(checkout), "rev-parse", "HEAD"]).stdout.strip()
        if not head.startswith(commit):
            sys.exit(f"{checkout} is at {head[:12]}, not {commit[:12]}")
        return
    checkout.mkdir(parents=True, exist_ok=True)
    sh(["git", "clone", "--filter=blob:none", "--sparse", "--depth", "1",
        "https://github.com/lowRISC/opentitan", str(checkout)])
    sh(["git", "-C", str(checkout), "fetch", "--depth", "1", "origin", commit])
    sh(["git", "-C", str(checkout), "checkout", "-q", commit])
    sh(["git", "-C", str(checkout), "sparse-checkout", "set", *SPARSE])


def search_dirs(root: Path):
    dirs = [root / "hw/ip/prim_generic/rtl"]
    dirs += sorted(
        p for p in (root / "hw/ip").glob("*/rtl")
        if p.parent.name != "prim_generic" and not p.parent.name.startswith("prim_xilinx")
    )
    dirs += sorted((root / "hw/top_earlgrey/ip_autogen").glob("*/rtl"))
    dirs += [root / "hw/top_earlgrey/rtl"]
    return [d for d in dirs if d.is_dir()]


def index_definitions(dirs):
    idx = {}
    for d in dirs:
        for f in sorted(d.glob("*.sv")):
            for _, name in DEF_RE.findall(f.read_text(errors="ignore")):
                idx.setdefault(name, []).append(f)
    return idx


def index_includes(dirs):
    idx = {}
    for d in dirs:
        for f in sorted(list(d.glob("*.svh")) + list(d.glob("*.sv"))):
            idx.setdefault(f.name, f)
    return idx


def run_slang(files, inc_dirs, top, slang):
    cmd = [slang, "--quiet", "--single-unit", "--top", top]
    for d in inc_dirs:
        cmd += ["-I", str(d)]
    cmd += [str(f) for f in files]
    p = subprocess.run(cmd, capture_output=True, text=True)
    return p.returncode, p.stdout + p.stderr


def blackbox_stub(root: Path, defs, name: str, out_dir: Path) -> Path:
    """`NAME_bbox.sv`: the upstream module header verbatim, `(* blackbox *)`, no body."""
    src_files = defs.get(name)
    if not src_files:
        sys.exit(f"--blackbox {name}: no upstream file defines it")
    src = src_files[0].read_text(errors="ignore")
    m = re.search(r"module\s+" + re.escape(name) + r"\b.*?\);", src, re.S)
    if not m:
        sys.exit(f"--blackbox {name}: could not read its port list from {src_files[0]}")
    text = (
        f"// LOCAL (mununu): black-box stub of OpenTitan {name} — the port list is the upstream header\n"
        f"// ({src_files[0].relative_to(root)}) verbatim; the body is deliberately absent and the module is\n"
        "// marked (* blackbox *), so the lift cuts every output to a free input: a declared\n"
        "// over-approximation (the module's datapath is not modelled).\n"
        "(* blackbox *)\n" + m.group(0) + "\nendmodule\n"
    )
    p = out_dir / f"{name}_bbox.sv"
    p.write_text(text)
    return p


def make_shim(real_dir: Path) -> str:
    """An empty definition for every macro of the real `prim_assert` set, plus what the RTL needs."""
    names, add = set(), []
    for n in ("prim_assert.sv", "prim_assert_standard_macros.svh", "prim_assert_sec_cm.svh"):
        txt = re.sub(r"\\\n\s*", " ", (real_dir / n).read_text(errors="ignore"))
        for m in re.finditer(r"^`define (\w+)(\([^)]*\))?", txt, re.M):
            name, params = m.group(1), m.group(2) or ""
            if name in names or name in ("INC_ASSERT", "PRIM_ASSERT_SV", "ASSERT_DEFAULT_CLK",
                                         "ASSERT_DEFAULT_RST", "ASSERT_RPT", "ASSERT_ERROR"):
                continue
            if name.startswith("_") and name != "_SEC_CM_ALERT_MAX_CYC":
                continue
            names.add(name)
            add.append(f"`define {name}{params}")
    head = (
        "// Synthesis shim for OpenTitan's prim_assert macro set — generated by\n"
        "// scripts/vendor_opentitan_ip.py from the real files: every assertion macro expands to NOTHING\n"
        "// (as OpenTitan's own prim_assert_dummy_macros.svh does), so the sv2v + yosys lift sees no SVA.\n"
        "// Extraction reads the REAL macros; this file replaces prim_assert.sv for the lift only\n"
        "// (`--lift-override prim_assert.sv=prim_assert_shim.sv`, the real .svh files blanked).\n"
        # no include guard — the hand-written shim never had one, and WITH one the sv2v + yosys lift of
        # edn fails (yosys: a topological loop; bisected 2026-10-18, mechanism not traced)
        "`ifndef ASSERT_DEFAULT_CLK\n`define ASSERT_DEFAULT_CLK clk_i\n`endif\n"
        "`ifndef ASSERT_DEFAULT_RST\n`define ASSERT_DEFAULT_RST !rst_ni\n`endif\n"
        "`define _SEC_CM_ALERT_MAX_CYC 30\n"
    )
    tail = (
        "// The real prim_assert.sv includes the flop macros; a per-file compilation unit (read_slang)\n"
        "// sees PRIM_FLOP_SPARSE_FSM only through this include.\n"
        '`include "prim_flop_macros.sv"\n'
    )
    body = "\n".join(a for a in add if not a.startswith("`define _SEC_CM_ALERT_MAX_CYC"))
    return head + body + "\n" + tail


def order_sources(files, primary, texts):
    pkgs, mods = [], []
    for f in files:
        if f == primary:
            continue
        kinds = {k for k, _ in DEF_RE.findall(texts[f])}
        (pkgs if kinds == {"package"} else mods).append(f)
    pkg_names = {n: f for f in pkgs for k, n in DEF_RE.findall(texts[f]) if k == "package"}
    deps = {f: set() for f in pkgs}
    for f in pkgs:
        for n in set(PKG_REF_RE.findall(texts[f])) | set(IMPORT_RE.findall(texts[f])):
            g = pkg_names.get(n)
            if g is not None and g != f:
                deps[f].add(g)
    ordered, seen = [], set()

    def visit(f, stack=()):
        if f in seen or f in stack:
            return
        for g in sorted(deps[f], key=lambda p: p.name):
            visit(g, stack + (f,))
        seen.add(f)
        ordered.append(f)

    for f in sorted(pkgs, key=lambda p: p.name):
        visit(f)

    def rank(p):
        if p.name in MACRO_SET:
            return 0
        if p.suffix == ".svh":
            return 1
        if p.name.endswith("_bbox.sv"):
            return 2
        return 3

    return ordered + sorted(mods, key=lambda p: (rank(p), p.name)) + [primary]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ip", required=True, help="IP name, e.g. edn (its rtl/ dir and top module)")
    ap.add_argument("--commit", required=True)
    ap.add_argument("--checkout", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--top", help="top module (default: the IP name)")
    ap.add_argument("--rtl", help="RTL dir relative to the checkout (default: hw/ip/<ip>/rtl)")
    ap.add_argument("--blackbox", action="append", default=[], help="module left out as a (* blackbox *) stub")
    ap.add_argument("--rewrite", action="append", default=[], metavar="FILE|FROM|TO",
                    help="one-line local rewrite, recorded in UPSTREAM_COMMIT.txt")
    ap.add_argument("--slang", default="slang")
    a = ap.parse_args()

    top = a.top or a.ip
    sparse_checkout(a.checkout, a.commit)
    root = a.checkout.resolve()
    rtl = root / (a.rtl or f"hw/ip/{a.ip}/rtl")
    primary = rtl / f"{a.ip}.sv"
    if not primary.is_file():
        sys.exit(f"primary not found: {primary}")
    dirs = search_dirs(root)
    defs, incs = index_definitions(dirs), index_includes(dirs)
    inc_dirs = [root / "hw/ip/prim/rtl", rtl]
    a.out.mkdir(parents=True, exist_ok=True)

    files = sorted(rtl.glob("*.sv"))
    for n in MACRO_SET:
        p = root / "hw/ip/prim/rtl" / n
        if p.is_file() and p not in files:
            files.append(p)
    stubs = [blackbox_stub(root, defs, n, a.out) for n in a.blackbox]
    files += stubs
    omit = set(a.blackbox)

    unresolved = set()
    for _ in range(80):
        rc, out = run_slang([f for f in files if f.suffix == ".sv"], inc_dirs + [a.out], top, a.slang)
        wanted = set(UNKNOWN_RE.findall(out))
        for m in INCLUDE_RE.finditer(out):
            wanted.add(Path(next(g for g in m.groups() if g)).name)
        wanted -= omit | unresolved
        added = 0
        for name in sorted(wanted):
            cands = [c for c in (defs.get(name) or ([incs[name]] if name in incs else [])) if c not in files]
            if not cands:
                unresolved.add(name)
                continue
            files.append(cands[0])
            added += 1
            print(f"+ {name:40s} <- {cands[0].relative_to(root)}", file=sys.stderr)
        for f in list(files):
            for inc in INC_RE.findall(f.read_text(errors="ignore")):
                name = Path(inc).name
                if name in omit or any(g.name == name for g in files):
                    continue
                if name in incs:
                    files.append(incs[name])
                    added += 1
                    print(f"+ {name:40s} <- {incs[name].relative_to(root)} (include)", file=sys.stderr)
                else:
                    unresolved.add(name)
        if added == 0:
            if rc != 0:
                errs = [l for l in out.splitlines() if "error" in l]
                print("slang still reports:\n  " + "\n  ".join(errs[:20]), file=sys.stderr)
            break

    texts = {f: f.read_text(errors="ignore") for f in files}
    ordered = order_sources(files, primary, texts)
    names = {}
    for f in ordered:
        if f.name in names and names[f.name] != f:
            sys.exit(f"basename collision: {f} vs {names[f.name]}")
        names[f.name] = f
        if f.parent != a.out:
            shutil.copyfile(f, a.out / f.name)

    rewrites = []
    for spec in a.rewrite:
        fname, src, dst = spec.split("|", 2)
        p = a.out / fname
        t = p.read_text()
        n = t.count(src)
        if n == 0:
            sys.exit(f"--rewrite: `{src}` not found in {fname}")
        p.write_text(t.replace(src, f"{dst} /* LOCAL (mununu): was {src} */"))
        rewrites.append(f"# LOCAL: {fname} — {n} line(s): `{src}` -> `{dst}`")

    (a.out / "prim_assert_shim.sv").write_text(make_shim(root / "hw/ip/prim/rtl"))
    (a.out / "sources.txt").write_text("".join(f.name + "\n" for f in ordered))
    lines = [f"# lowRISC/opentitan @ {a.commit}",
             "# <file>  <upstream path>  (vendored byte-exact by scripts/vendor_opentitan_ip.py)"]
    for f in ordered:
        if f.parent == a.out:
            lines.append(f"{f.name}  LOCAL: (* blackbox *) stub, header verbatim, no body")
        else:
            lines.append(f"{f.name}  {f.relative_to(root)}")
    lines.append("prim_assert_shim.sv  LOCAL: generated from hw/ip/prim/rtl/prim_assert*.{sv,svh} (lift only)")
    lines += rewrites
    (a.out / "UPSTREAM_COMMIT.txt").write_text("\n".join(lines) + "\n")
    srcs = " ".join(f"--source {f.name}" for f in ordered if f != primary)
    (a.out / "run.sh").write_text(
        "#!/bin/sh\n"
        f"# `sv verify-auto` on the full {a.ip} IP (lowRISC/opentitan {a.commit[:9]}); run in mununu-sva, cwd = this dir.\n"
        "# Extraction reads the real prim_assert macros; the lift sees the synthesis shim (assertions compiled out).\n"
        "# Add `--source mununu_properties.sv` for the system-level @mununu_guarantee properties.\n"
        f"exec mununu sv verify-auto {primary.name} --top {top} --preprocess-sv2v {srcs} \\\n"
        "  --lift-override prim_assert.sv=prim_assert_shim.sv \\\n"
        "  --lift-override prim_assert_standard_macros.svh=/dev/null \\\n"
        "  --lift-override prim_assert_sec_cm.svh=/dev/null \\\n"
        "  --config-value rst_ni=1 \"$@\"\n"
    )
    print(f"{len(ordered)} sources -> {a.out}; unresolved: {sorted(unresolved) or 'none'}; slang rc={rc}")


if __name__ == "__main__":
    main()
