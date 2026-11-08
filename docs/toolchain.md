# Toolchain

> Concept: how the Rust version is pinned across host, container, and CI — and what clippy patterns to keep in mind so toolchain bumps don't break the gate.

## Where the version lives

The workspace pins an exact Rust version in two places:

- [`rust-toolchain.toml`](../rust-toolchain.toml) — `channel = "1.95.0"`. rustup honors this at runtime, so host devs, CI, and the dev container all use the same toolchain.
- [`docker/Dockerfile.dev`](../docker/Dockerfile.dev) — `ARG RUST_VERSION=1.95`. Mirrors the toolchain pin so the base image's bundled toolchain matches.

Edition is **2024** (set in each `Cargo.toml`).

## Z3 on each OS

> Source of truth: [`os-matrix.yml`](../.github/workflows/os-matrix.yml) (the three per-OS Z3 steps) and [`z3`](../crates/mununu-core/Cargo.toml) in `mununu-core`'s dependencies — surface: CLI+API (build-time; every verb that reaches the native BMC / k-induction or interpolation engines links Z3).

Z3 is a **linked library** (`z3 = "0.21"`, `z3-sys`): the build links the system `libz3`, found
through pkg-config or a link-search path, and reads its version from pkg-config or from `z3
--version` on `PATH`. That is where every supported host gets it — `apt-get install libz3-dev` on
Linux and in the dev image, `brew install z3` on macOS. Windows has no system route, so the
Windows lane of the OS matrix unpacks the Z3 project's official `x64-win` release zip and hands it
to `z3-sys` with no cargo feature: `Z3_LIBRARY_PATH_OVERRIDE` names its `bin/` (where the import
library `libz3.lib` is copied to `z3.lib`, the name the crate links), and `bin/` goes on `PATH` for
`z3.exe` (version detection) and `libz3.dll` (the tests load it). A self-hosted Windows build does
the same by hand. `z3-sys` also offers `vendored` (source build) and `gh-release` (prebuilt
download) features, but they are **mutually exclusive in its build script**, so this repository
defines no cargo feature for either — one would break every `--all-features` build the hooks and
the pre-push check run. The library is the same on every route; none of this touches a verdict.

**The OS matrix** ([`.github/workflows/os-matrix.yml`](../.github/workflows/os-matrix.yml), roadmap
I1) builds and tests the workspace natively on Linux, macOS and Windows — the engine-only tier (T2):
no SV toolchain, no e2e — on every push to `main`, on release tags, on demand, and on a PR only when
the PR touches the workflow or its scripts (macOS minutes cost 10× Linux). Each lane's test count
passes `scripts/test_count_gate.sh`, and `scripts/os_matrix_compare.sh` then requires the three
counts to agree within the allowance of
[`measurements/os-matrix-unix-only-tests.txt`](../measurements/os-matrix-unix-only-tests.txt) (the
tests that test a Unix-only mechanism, by name). A lane that runs fewer tests and goes green is
C3's bug in a new coat; a test is made to run everywhere, never `cfg`-skipped to go green.

**First measurement (2026-11-08, the PR that added the workflow):** Linux 3746 tests passed, macOS
3746 — identical suites; Windows built, linked Z3 from the zip, and ran 3740: three tests failed
on the host separator or a CRLF checkout (fixed in the same PR: a test split a rendered path on
`/`, a test spelled a `/`-joined expectation for a path the code joins with the host separator, a
baseline file was compared byte-wise against an LF snapshot), and three tests do not exist there —
the two `#![cfg(unix)]` binaries that drive a process group and a `/bin/sh` launcher — which is what
the allowance file names, each with its issue.

## Bumping the toolchain

A toolchain bump is a two-file edit plus a `make ci` validation, all in one commit:

1. Edit `rust-toolchain.toml`'s `channel`.
2. Edit the matching `ARG RUST_VERSION` in `docker/Dockerfile.dev`.
3. Run `make ci` locally. New clippy lints often surface on bumps — fixing them is part of the bump commit, not a follow-up.

**Drift between the two pins is silent.** rustup will auto-download the toolchain that `rust-toolchain.toml` names regardless of the image's bundled version, so CI may pass against a "wrong" toolchain. Treat any PR that touches one pin but not the other as a review red flag.

Before committing the pin change, run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

against the new toolchain so any new lint failures land in the same commit.

## Clippy compatibility patterns

The pinned toolchain locks the lint set so contributors don't get bitten by surprise lints from upstream Rust releases. Keep these patterns in mind when writing new code — each has bitten a past bump:

- **`unnecessary_unwrap`.** After `x.is_some()`, use `if let Some(v) = x` rather than `x.unwrap()`.
- **`needless_return`.** Don't use explicit `return` at the end of a function body.
- **`redundant_closure`.** Use `foo` instead of `|x| foo(x)` when passing to `.map()` and friends.
- **`collapsible_match`** (Rust 1.95+). A nested `if` inside a `match` arm should become an arm guard — `Pat if cond => { ... }` rather than `Pat => { if cond { ... } }`.
- **`implicit_borrowing`** (Edition 2024). Closure patterns like `|(&(a, _), _)|` are not allowed in Rust 2024 — use `|((a, _), _)| *a` instead.

For the most recent bumps and what they revealed, check the git log on `rust-toolchain.toml`.
