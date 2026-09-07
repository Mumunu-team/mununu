# Consumer briefing — 2026-08 antecedent shadow-register synthesis (mununu#476)

> **Audience:** two downstream consumers of mununu — **ROSF** (subprocess consumer via `--profile industrial`) and **monono** (direct CLI consumer). One document, two role-branching sections + a shared footer. Read the section that applies to your repo, then the footer.
>
> **Fix:** mununu#476. **Design:** [`docs/design/antecedent-shadow-synthesis.md`](../design/antecedent-shadow-synthesis.md). **User-facing docs:** [`docs/verifying-rtl.md`](../verifying-rtl.md) §"SVA `|=>` with input-derived antecedents".
>
> **TL;DR:** the exact-symbolic engine now auto-synthesises antecedent shadow registers for SVA `|=>` properties whose antecedent reaches primary inputs. Verdicts that previously came back `Skipped` (or, before Phase A, unsoundly `Violated (1 cell)` on correct RTL) now decide. Behaviour change is a **strict improvement** — soundness never regressed.

## What changed at engine level

Before this fix (Phase A, `#476` transitive refusal): SVA `A |=> C` whose antecedent `A` was combinationally driven by primary inputs (`mem_rvalid_mine = mem_rvalid && (mem_rid == CLIENT_ID)`, `valid_and_ready = valid && ready`, address decoders, enable stacks) returned `Skipped` with a refusal message naming the source inputs.

After this fix (Option C, `#476` shadow-synthesis): the same properties **decide**. The exact-symbolic engine detects the canonical `|=>` lift shape in the mu-calc formula, synthesises a shadow state register `_mununu_antshadow_<N>` that samples `A` per cycle (`init = 0`, `next = A`), and rewrites the atom to reference the shadow before evaluation. Standard SVA-to-BMC compilation technique.

Five fallback conditions still hit the Phase A refusal (definite `Skipped`, never wrong): non-Boolean antecedent, array/memory in cone, atom is itself a primary input, cone reaches an anonymous free input from partial-write havoc, multi-atom antecedents `(a && b) |=> c`. Everything else that used to `Skipped` on this path now decides.

**Opt-out:** the `MUNUNU_NO_ANTECEDENT_SHADOW=1` environment variable disables shadow-synth (reverts to the Phase A refusal). Debug / differential-oracle use only.

---

## For ROSF-agents (subprocess `--profile industrial`)

**What to update on your side:** nothing required if your verdict-parsing keys on `PropertyVerdict` values (`Holds` / `Violated` / `Unknown` / `Skipped`) — those are unchanged.

**What to expect:** properties on RTL that use ready-valid, address decoding, or any combinational-of-input antecedent (very common in bus/handshake modules) will start returning `Holds` / `Violated` / `Unknown` where they previously returned `Skipped`. Your `--profile industrial` gate becomes tighter — that's the point.

**If your report parser looks at the `Skipped` refusal message text** (unusual but possible): the message strings for the fallback conditions listed above are unchanged. The pre-existing "atom references primary input" and "combinationally driven by primary input(s)" messages both still occur when a fallback fires. Nothing new to parse — new decides use the same `PropertyVerdict::Holds/Violated/Unknown` shape as any other decided property.

**Docker rebuild:** rebuild `rosf/docker/Dockerfile` **only if you pin a specific `mununu` binary version tag**. If you fetch the latest mununu binary on image build, the behaviour change picks up automatically on your next rebuild. `rosf/Dockerfile.dev` and `rosf/Dockerfile.hw` are unaffected — they don't invoke mununu.

**Test the transition:** run your `--profile industrial` gate against any design that previously reported a `Skipped` verdict with the phrase "combinationally driven by primary input" in the refusal message. Those should now decide. If any of them come back `Violated` when your intent was `Holds`, that's a signal to inspect the RTL — mununu was previously silent on that property, now it's speaking.

---

## For monono-agents (direct `sv verify-auto` CLI consumer)

**What to update on your side:** nothing required. Just re-run `sv verify-auto` on your tree.

**What to expect:** the `wb_mem_client`-shaped case from mununu#476 (`mem_rvalid_mine = mem_rvalid && (mem_rid == CLIENT_ID)`, SVA `((st_q == 3) && mem_rvalid_mine && got_lo_q |=> (st_q == 4))`) now DECIDES instead of refusing. The workaround in the mununu#476 report (restate the property over registers only) is no longer needed — the original property works. Consider whether to revert the workaround in your RTL/property set.

**Everywhere else this shape appears** in the monono tree — bus protocols, handshake gating, enable stacks, address decoders with input-dependent antecedents — the same story. Properties that were reporting `Skipped` on this exact shape will now report a definite verdict.

**Docker rebuild:** monono's Docker (if any) needs rebuild **only if it pins a specific mununu binary version**. Otherwise the behaviour change picks up on the next binary pull.

**Test the transition:** replay the `#476` case from the report — the `mem_rvalid_mine` fixture. Confirm the verdict is now definite rather than `Skipped` with the transitive-fan-in refusal message. Also inspect any property in your tree currently marked `Skipped` on `sv verify-auto`; the ones that were caught by the transitive refusal will now decide.

**Related open issue #475** — the five `sv lint` / `sv mutate` ergonomics gaps from your adoption report are **NOT fixed in this PR**. Those are a separate follow-up (see the earlier issue-sweep plan at `.claude/plans/i-am-looking-the-declarative-snowflake.md`). This PR is scoped to #476 only.

---

## Docker rebuild table (both consumers)

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod CLI) | binary carries new engine behaviour | Yes if consumers pin a version tag |
| mununu `Dockerfile.dev` | binary bump | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; e2e tests exercise the shadow-synth path | **Yes** — rebuild + full slang-gated e2e test run under this image before merge (per CLAUDE.md §"SVA-verification e2e validation") |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `docker/Dockerfile` | consumes mununu subprocess; verdict quality changes on input-derived antecedents | Only if pinned to a version tag; behavior updates on next binary pull otherwise |
| rosf `docker/Dockerfile.dev`, `.hw` | no direct dependence on the changed paths | No |
| monono Docker (any) | consumes `sv verify-auto` output | Only if pinned to a version tag |

---

## Shared footer — verification you should run after adopting

1. **Confirm your verdict-parsing still works** on a property that previously returned `Skipped` under the transitive refusal. It should now return `Holds`/`Violated`/`Unknown` with the standard payload shape.
2. **Confirm the opt-out escape hatch works** (only relevant if you build a differential oracle): set `MUNUNU_NO_ANTECEDENT_SHADOW=1` before invoking `sv verify-auto`, verify the same property returns to the Phase A refusal. This is your emergency knob if you ever need to compare shadow-synth verdicts against the pre-shadow behaviour.
3. **If your CI gate expected `Holds` on any of the newly-decided properties, treat the change as a soundness update.** Your gate was passing on `Skipped` (which the CI-gate exit code counted as pass under `--fail-on violated`); it will now pass on a real `Holds`. If it comes back `Violated`, the RTL has a bug the previous engine could not report on.

## Provenance

- Fix commit: (pending merge — see branch `fix/476-antecedent-shadow-synth` in the mununu repo)
- Issue: mununu#476 — <https://github.com/Mumunu-team/mununu/issues/476>
- Design: `docs/design/antecedent-shadow-synthesis.md`
- Policy this briefing was written to satisfy: `docs/policies/cross-repo-impact.md`

## Not covered here (follow-ups)

- mununu#475 (five `sv lint` / `sv mutate` ergonomics gaps) — separate PR, targeted next.
- Multi-atom antecedents `(a && b) |=> c` — a scope extension of the shadow-synth pass, targeted after real-world evidence one is needed.
- Stable JSON schema doc for `PropertyVerdict` / `DiscoveredAssumption` / `AutoVerifyReport` — planned as `docs/api-schemas/verdict.md`; not yet written. When it lands, both ROSF and monono get a schema URL to pin their parsers against.
