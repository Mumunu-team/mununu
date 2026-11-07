---
name: docs-traceability
description: >
  Verifies that mununu's documentation (wiki/, docs/, README.md, examples
  README files, agent and skill prompts) anchors each feature description
  to a live code artifact reachable from the CLI, HTTP API, or UI.
  Reports orphan sections (no anchor), broken anchors (path/symbol does
  not exist), and reachability gaps (anchored symbol is dead code or no
  surface exposes it). Use when reviewing documentation changes, or as
  a sub-step of review-orchestrator and domain-adequacy.
---

Run a documentation traceability audit on $ARGUMENTS (a path, file list, or "changed files" if no args). The rule being enforced is `CLAUDE.md` → Governance Rules → **Documentation Traceability**.

## Scope determination

If $ARGUMENTS is empty, derive scope from `git diff --name-only HEAD~1 HEAD` filtered to `*.md` files under the covered paths. Otherwise, treat $ARGUMENTS as either:

- A path or directory (e.g., `wiki/`, `docs/abstraction.md`) — audit every Markdown file under it.
- A feature/symbol name — find every doc page that should anchor to it and check those.

## Covered paths

| Path | What lives here |
|---|---|
| `wiki/**/*.md` | Public wiki — every page must anchor |
| `docs/**/*.md` | In-repo documentation — every page must anchor unless tagged `> Status: planning` |
| `README.md`, `crates/*/README.md` | Project + crate READMEs |
| `CLAUDE.md` | The project instructions — every `docs/` or `wiki/` path it cites must exist and be tracked (roadmap D3; it was outside the scope until 2026-11-07) |
| `examples/**/README.md` | Example walkthroughs — anchor to the example file path and the CLI/API/UI commands that load it |
| `.claude/agents/**/*.md` | Agent prompts — anchor each capability claim |
| `.claude/skills/**/SKILL.md` | Skill prompts — anchor file/symbol references in the "Procedure" section |

Explicit opt-outs:
- `docs/archive/**` (historical docs restored for the references that cite them; each carries `> Status: historical` and is not anchored)
- `docs/design/native-sv-abstraction.md` and every file starting with `> Status: planning` (planning docs)
- `wiki/Home.md`, the table-of-contents page (anchor target is each linked page, not the index)

## Anchor grammar

The skill recognizes two anchor styles. A section is considered anchored if it carries one of these lines within the first 5 lines after its heading:

```markdown
> Source of truth: [`<symbol or path>`](<relative path>#L<line>) — surface: CLI | API | UI | CLI+API | CLI+API+UI | CLI-only — <justification>

> Concept: <one-line — exempt; section explains a textbook concept, not a feature>

> Status: planning   (file-level — exempt; goes at top of file, not per-section)
```

The skill validates:

1. **Path is tracked** — the `<relative path>` resolves from the repo root to a file **git tracks** (`git ls-files --error-unmatch <path>`). Present-but-untracked is `path-untracked`: a gitignored path, or one served by a local symlink (the untracked `plans` tree under `.claude` on a machine with the private sync), exists for the author and for nobody who clones the repository (roadmap D3/G4, 2026-11-07).
2. **Symbol exists when given** — if the anchor names a Rust symbol (`fn foo`, `struct Foo`, `enum Foo`, `const FOO`), grep for `(fn|struct|enum|const|trait|type)\s+<symbol>` in the cited file. If a line number is given, the symbol must appear within ±5 lines.
3. **Surface tag is well-formed** — must be one of `CLI`, `API`, `UI`, `CLI+API`, `CLI+UI`, `API+UI`, `CLI+API+UI`, `CLI-only`, `API-only`, `UI-only`. Single-surface tags require a `— <justification>` suffix.
4. **Surface reachability** — for each declared surface, the symbol or one of its callers must appear in that surface's entry points:
   - **CLI** entry point: `crates/mununu-cli/src/main.rs` (clap derive args + handler dispatch)
   - **API** entry point: `crates/mununu-core/src/api/server.rs` (route table) + `handlers.rs` (handler bodies)
   - **UI** entry point: `mununu-ui/src/api/endpoints.ts` (typed client) + any hook under `mununu-ui/src/hooks/`

   Reachability is satisfied if `git grep` finds the symbol name (or a parent module that re-exports it) in the surface's entry-point files, or in any file transitively imported by them. Skip transitive resolution if a direct mention is found.

## Procedure

1. **Discover sections.** For every Markdown file in scope, split on `^## ` and `^### ` headings. The file-level `> Status: planning` directive (if present in the first 5 lines of the file) exempts every section in the file.

2. **Classify each section.** For each H2/H3 section, look at the first 5 non-empty lines after the heading and classify:
   - **Anchored**: matches the `> Source of truth:` grammar.
   - **Concept**: matches `> Concept:`.
   - **Orphan**: no anchor and no concept tag.

3. **Validate anchors.** For each Anchored section, run the four checks above. Record failures as:
   - `path-missing` — cited path does not exist
   - `path-untracked` — cited path exists on this machine but git does not track it
   - `link-untracked` — a Markdown link (not an anchor) into a path git does not track
   - `symbol-missing` — cited symbol not found in the path (or not within ±5 lines of the cited line)
   - `surface-malformed` — surface tag not in the allowed set, or single-surface tag missing justification
   - `surface-unreachable` — declared surface does not expose the symbol

3a. **The mechanical pass already runs in CI.** `crates/mununu-core/tests/docs_references_resolve_d2.rs` fails when a `docs/…md` or `wiki/…md` path cited from `CLAUDE.md`, `README.md`, the crates, `examples/`, `docs/` or `wiki/` does not exist; `crates/mununu-core/tests/no_links_into_untracked_paths_g4.rs` fails when a covered document points into the untracked `plans` or `reviews` trees under `.claude`, or into a directory the root `.gitignore` names. Read their output first; this skill adds the anchor grammar, symbol and surface checks the tests do not make.

3b. **Validate prose links.** Every Markdown link in a covered file whose target is a repository path (not a URL, not `mununu-private/…`) must resolve to a git-tracked file or directory; a link into a gitignored path (the untracked `plans` and `reviews` trees under `.claude`, `experiments/`) is `link-untracked`. The G4 audit of 2026-11-07 found 44 such links and 0 broken anchor lines: the links were the whole finding, and anchor-only validation saw none of them.

4. **Flag orphans.** Every Orphan section is a finding. Suggest one of: (a) add an anchor, (b) mark `> Concept:`, (c) hoist the file to `> Status: planning`, (d) delete the section.

5. **Cross-check with parity-check (when both run in sequence).** If `/parity-check` ran in the same review, cross-reference: a section anchored `surface: CLI+API+UI` must agree with parity-check's verdict for that feature. Disagreements (e.g., docs claim UI exposure but parity-check finds no UI client) are reported as `surface-disagreement` and resolved in favor of parity-check.

## Report format

Emit a Markdown table. The caller (an agent or a review report) can quote it verbatim.

```markdown
### Documentation Traceability

Audited N Markdown files across {wiki, docs, READMEs, agents, skills}. M sections anchored; K orphan; J broken.

| File:section | Status | Anchor | Surface | Issue |
|---|---|---|---|---|
| `wiki/CLI-Reference.md` § `mununu context eval` | OK | `crates/mununu-cli/src/main.rs#L412` (`EvalArgs`) | CLI+API+UI | aligned |
| `wiki/Adapter-Formats.md` § TLSF | BROKEN | `crates/mununu-core/src/adapter/tlsf/mod.rs#L88` (`fn parse_tlsf`) | CLI+API | symbol-missing — moved to `parser.rs:L42` |
| `docs/abstraction.md` § "Boolean domain" | ORPHAN | — | — | add anchor or mark `> Concept:` |
```

Each row must cite the path used as the anchor for OK rows. BROKEN / ORPHAN rows must include a one-line remediation in the **Issue** column.

If the audit is clean (every section anchored or correctly tagged), the table contains only OK rows and the executive line reads `M sections anchored; 0 orphan; 0 broken — clean`.

## When invoked as a sub-step

If the caller is `review-orchestrator` or `domain-adequacy`, return only the Markdown table block plus the one-line executive summary — no preamble, no closing notes. The caller embeds it directly under a `### Documentation Traceability` section in its larger report.

When invoked standalone, prepend an executive paragraph: total files audited, total sections, anchored/orphan/broken counts, and the highest-priority remediation (e.g., "5 wiki pages are completely unanchored; start there").

## Important

- A pre-existing traceability gap discovered during the audit is itself a finding. Report it under the same table even if it predates the current change.
- The skill is read-only. It does not modify documentation, does not add anchors, and does not delete sections. The caller decides what to fix.
- When a section legitimately documents a *removed* feature (e.g., "Legacy flag X — removed in 0.5"), mark it `> Concept:` with a one-line "removed in commit/tag" reason; do not anchor to nonexistent code.
- Do not anchor to files under `mununu-private/` or any path in `.gitignore` — those are not reachable to readers of the public repo.
