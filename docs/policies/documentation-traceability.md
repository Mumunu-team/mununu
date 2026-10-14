# Documentation traceability

> Status: policy (binding for `wiki/**`, `docs/**`, `README.md`, `examples/**/README.md`, and the `description:` / inline guidance of `.claude/agents/**` and `.claude/skills/**`). `CLAUDE.md` carries the rule in short form; this file is the full text.

## The rule

> Source of truth: [`.claude/skills/docs-traceability/SKILL.md`](../../.claude/skills/docs-traceability/SKILL.md) — surface: CLI-only — a review skill, run from Claude Code

Every documentation page or section that describes a feature, flag, endpoint, file format, syntax form or behaviour must be traceable to a *live* code artifact — a Rust `struct` / `enum` / `fn`, a clap arg, an axum handler, a TypeScript client, or a checked-in configuration file — reachable from at least one of the three user-facing surfaces (CLI, HTTP API, UI). Documentation that cannot point at such an artifact is obsolete, aspirational or unverifiable, and is marked or removed. Doc rot is the single most common reason users distrust the tool; anchoring docs to symbols lets a reviewer grep for drift the moment code is renamed, removed or made unreachable.

**Anchor format.** Every H2 / H3 section documenting a concrete capability carries, near its top:

```markdown
> Source of truth: [`<symbol or path>`](<relative path>#L<line>) — surface: CLI | API | UI | (CLI+API+UI)
```

The path is a real file in this workspace; line numbers are optional. A single-surface tag needs an inline justification (`surface: CLI-only — <reason>`).

**Not covered.** Pedagogical sections (`> Concept: <one-line>`); planning or vision documents (`> Status: planning` at the top of the file); tutorial prose that anchors to the example file it walks through.

**Alive** means referenced (not just declared) by code reaching a surface, or a configuration file consumed by a shipping binary or test target. **Not acceptable:** `#[allow(dead_code)]` symbols, deleted-test references, non-default feature flags no surface enables, files outside the repo, closed-source dependencies, `mununu-private/`.

**Drift detection.** `/docs-traceability` walks every Markdown file under the covered paths, parses each `> Source of truth:` line and verifies the anchor resolves, the symbol exists at the cited line and the surface tag matches a reachable mention on that surface. Broken anchors fail; new sections without an anchor fail unless tagged.

**Renames and removals.** The commit that renames or removes a symbol updates every anchor that points at it. `/parity-check` confirms a *feature* is wired across surfaces; `/docs-traceability` confirms a *page* still points at code that exists. Both pass before a change ships.

## Cadence

> Source of truth: [`scripts/docs-audit.sh`](../../scripts/docs-audit.sh) (`make docs-audit`) — surface: CLI-only — a contributor instrument

When more than **10 commits** have landed on `main` since the last commit touching `docs/`, `wiki/`, `README.md` or `examples/`, the next commit either includes a `docs/` or `wiki/` delta capturing the accumulated changes, or runs `/docs-traceability` and records the verdict (in the commit message or a `.claude/reviews/docs-traceability/` note).

```bash
make docs-audit                       # default threshold = 10 commits
make docs-audit DOCS_THRESHOLD=20
```

The script reports the commit count since the last docs touch, whether the threshold is exceeded, a categorised breakdown of which code areas saw commits (CTXDSL syntax, API surface, CLI surface, composition, formula operators, adapters), and a reminder when `api/models.rs` was touched (mununu-ui's `src/api/types.ts` may need a matching update — there is no typegen between the two repos). Advisory: exit 1 on breach, never a CI or hook failure. When it fires, run `/docs-traceability` first — anchor-by-anchor verification surfaces the most actionable drift; adding sections for new code is the follow-up.
