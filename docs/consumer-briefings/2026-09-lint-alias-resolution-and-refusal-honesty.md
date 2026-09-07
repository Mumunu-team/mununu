# Consumer briefing — 2026-09 `sv lint` sees reset-bearing registers; the partial-write refusal stops guessing its cause

> **Audience:** monono (reported both), ROSF, anyone running `mununu sv lint` or reading a `verify-auto` `Skipped` reason.
>
> **Related:** [mununu#506](https://github.com/Mumunu-team/mununu/issues/506), [mununu#507](https://github.com/Mumunu-team/mununu/issues/507). Follows #496 / #502.
>
> **TL;DR:** the #496 registered-read rule was **near-vacuous** — it fired only on registers with *no reset*, which is almost none. Fixed. And the partial-write refusal asserted a cause it could not see, pointing consumers at correct RTL; it now reports what it actually knows. **`sv lint` will report findings it previously missed.**

## 1 — The registered-read rule was blind to almost all real RTL (#506)

Reported as a **cross-module** blind spot. It is not: mununu's lift **flattens**, so the flattened netlist contains the whole pattern. The rule failed one step later.

`async2sync` lifts an async reset into a mux, so what the read indexes is not the register:

```
10 state 8              <- the address register (unnamed)
11 ite 8 4 10 9         <- rst_n ? state : 0     the RESET MUX
12 uext 8 11 0 ld_q     <- the NAME lives here, on an alias of the mux
17 read 5 16 11         <- the read indexes nid 11, NOT the state nid 10
```

The rule required a bare `state` and bailed. **The discriminating experiment** — the same single-module design that #496 catches, with a reset added and nothing else changed:

| | findings |
|---|---|
| address register with **no** reset | 1 |
| address register **with** a reset, one module, no boundary | **0** |

So the blind spot was never the module boundary. **The rule fired only on reset-less registers.** That is why it was clean on all six of monono's memory blocks and on the loader.

### What changed

The address, the tracking register, and the reported name all resolve back through the identity-preserving wrappers (`uext` / `sext` / `slice` / a reset-shaped `ite`).

**Both sides had to move together.** `a_d <= a_q` also lifts to `next(a_d) = ite(rst, <alias of a_q>, 0)`. Resolving only the address would have stopped the rule recognising the *satisfying* form and made it fire on exactly the designs that already do the right thing — strictly worse than the original silence. A reset-bearing satisfying twin is now a first-class regression, in the unit tests and through the real slang lift.

Arithmetic is still not walked, so `mem[a_q + 1]` remains out of scope, as documented — with a regression pinning that too.

**Because the lift flattens, the cross-module form is now covered** — array and destination register in a child, address register in the parent. That is the shape that motivated #496, and it now flags.

### What to expect

**`sv lint` will report findings it previously missed**, and on real RTL that is most of the fault class. Run it and expect hits:

```bash
mununu sv lint <rtl> --frontend slang
```

Each hit is either a real mis-pairing or a place where the correspondence is recorded in a way the check cannot see. The second case is a false positive worth reporting so it can be narrowed — the rule recognises exactly one satisfying form (a register whose `next` resolves to the address register).

## 2 — The partial-write refusal asserted a cause it could not see (#507)

The guard's actual condition is **"the property's atom has a cone reaching an unnamed free input."** The message hardcoded one cause:

> *…while lifting a partial register assignment (`{reg}[hi:lo] <= …`)*

An unnamed free input has several sources: a partial write, a **black-boxed submodule's freed outputs**, an undriven net, an unconnected port. On an integrator wiring ~25 submodules, black-boxing is far likelier. monono's RTL confirms the misattribution — `f_slot_q` is written whole in both branches, and `f_slot[6:0]` is a part-select of the *source*.

**Now:** the message leads with the condition, and names the cause only when the lift shows it. The partial-write shape is detectable (the register's name aliases a `concat` mixing a free input) and is still reported as such. Otherwise the message says the cause is **not attributed**, lists the possibilities, and — when the run black-boxed anything — **names those modules**, which is the likely source and was already in the diagnostics.

## 3 — The suggested remedy could not run (#507)

The message recommended `--frontend verilog --preprocess-sv2v`. For a bound-SVA design that is not merely awkward — **sv2v 0.0.13 cannot parse `bind` at all**:

```
$ sv2v dut.sv dut_sva.sv
dut_sva.sv:4:1: Parse error: unexpected token 'bind' (KW_bind)
```

(monono hoped sv2v could elaborate it away; it cannot, at this version.) And `bind` is not optional for them: `read_verilog -sv` cannot parse `assert property`, and `verify-auto` has no `--define`, so an `ifdef` guard would hide the assertions from the verifier too.

**Now:** the sv2v remedy is offered only when the partial-write shape is actually present *and* the sources do not use `bind`. For a bound-SVA design the message says the remedy is unavailable and why, and points at the route that does work (write the register whole). An honest dead end beats a false lead.

## What did NOT change

- Verdicts. The #507 work is message-only; no property changes outcome.
- The `undriven-partial-write` lint rule, its fixture, and the `sv lint` exit codes.
- `signal`, `kind`, `rule`, `detail` field shapes; routes; CLI flags.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | new lint findings; corrected refusal text | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump; the e2e regressions run here | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no lint / verify-auto path | No |
| rosf | reads refusal text if surfaced | **No** — rebuild when adopting |
| monono Docker | reported both | **Yes** |
| mununu-ui | no type change | No |

## Verification

```bash
cargo test -p mununu-core --lib -- sv_verify partial_write_shape bound_sva_sources

docker run --rm -v "$(pwd)":/work -v mununu-target:/ct -w /work -e CARGO_TARGET_DIR=/ct \
  mununu-sva bash -c 'export PATH=$HOME/.cargo/bin:/opt/oss-cad-suite/bin:$PATH; \
    cargo test -p mununu-core --lib e2e_sv_lint -- --ignored'
```

The e2e lifts the reset-bearing faulty design **and its satisfying twin** with real yosys-slang and asserts the first flags and the second does not.

## Still open, and worth knowing

- ~~**`sv lint` skips every dotted (`u_inst.sig`) Op symbol.**~~ **Fixed** in the same series. The filter existed to suppress `<function>.<arg>` false positives (mununu#475), but on a flattened integrator *every* hierarchical alias is dotted, so it suppressed the whole design — which is why `sv lint` reported 0 findings while `verify-auto` refused on the same file. They were not disagreeing; `sv lint` was not looking. It now skips a dotted symbol only when no prefix of it names an instance scope, discriminated by the fact that **a function has no registers**: a prefix appearing on a `state` symbol names an instance. The #475 fixture is retained as the false-positive guard. Residual: an instance with no state at all is still skipped. **Expect additional `sv lint` findings on hierarchical designs.**
- **Declaration-order divergence.** monono found `video_sdram.sv` / `video_out_core.sv` used 47 and 12 signals *above their declarations*; verilator accepts this, slang rejects it, and bound SVA forces the slang lift. Such a file is invisible to both `sv lint` and the formal lane, and a verilator-first lint lane cannot see the gap. Worth recording in the RTL-frontend docs as a known divergence.
- [mununu#504](https://github.com/Mumunu-team/mununu/issues/504) — the exact engine can stack-overflow rather than abstain on a large design.
