# Private files: what never enters the public repositories

> Status: policy (binding). `CLAUDE.md` carries the rules in short form under "Private Files Policy"; this file is the full text with the incidents that produced them. Written 2026-11-05 from the exposure audit of the public `mununu` and `mununu-ui` repositories (working trees, full history, the host machine, the published artifacts). The audit found no secret, credential, key or licence fingerprint anywhere; everything below is about names, paths and unpublished material.

## The rule

> Source of truth: [`.gitignore`](../../.gitignore) (the excluded paths) — surface: CLI-only — a repository instrument, not a shipped verb

Sensitive or unpublished material lives in the sibling `mununu-private/` repository, never here: paper sources, benchmark scripts and expected outputs, extraction specs with CVE data, protocol CTXDSL models, MCP-extracted CTXDSL, governance scripts, tutorials, evaluation data, experiment archives, write-ups that have not been published, machine-local configuration, and the engagement, business and operations notes. The public repositories carry the Rust source, the adapters and their formats, the extraction framework's *capabilities* (adapter code, validation, schema), the shared IR, the tests, the README, the wiki, the design notes and the policies.

Before adding a file here that you would not want public: move it to `mununu-private`, add its path to `.gitignore`, and leave an origin note beside the private copy (where it came from, the last public commit that carried it, how to restore it where a `make` target still expects it). A `git rm --cached` plus the ignore entry keeps the file on disk only in the checkout that made the commit; every other checkout loses it on the next pull, so the private copy comes first.

## Rule 1 — names

Partner, customer, vendor, prospect, engagement and person names stay in `mununu-private`. They do not appear in tracked files, code comments, commit messages, branch names, or GitHub issue and PR titles, bodies and comments. What may be named: a consumer by its repository (rosf, monono, mununu-ui), a public upstream project (OpenTitan, …), a public tool, the public committer identity, and an origin by its public issue number. A consumer briefing's provenance line says "found preparing the tutorial recordings" or "a partner meeting", never who the recordings or the meeting were for; a code comment says "the tutorial `cmd_ctrl` design", never whose design it is.

**Why.** The 2026-11 audit found three engagement names in 18 tracked files and 23 commits of `mununu` (none in `mununu-ui`). The vector was provenance prose, written one honest sentence at a time: eleven consumer briefings' "found preparing the … tutorial recordings" lines, two code comments naming the partner whose module exposed a bug, a design note naming whose videos a release was for, an experiment README naming a meeting. One issue and fourteen PR bodies on GitHub repeated them. The people's names, the vendor's name and the codename of the largest engagement never reached a tracked file, because they had been declared private from the start; the three that leaked had not. The list of private names is kept in `mununu-private/docs/security/private-names.txt`; grep the tree against it before opening a PR.

## Rule 2 — machine-local files

No tracked file embeds an absolute path, a home directory, an account handle, a hostname or a non-loopback address. The classes that do, and stay out: launchd and systemd units, profiler output (dhat), sweep and frontier JSON, hardware fingerprints, criterion archives, editor and tool settings, `.claude/settings.local.json`. Docker volume and image names, and relative references to `mununu-private/`, are fine.

**Why.** The same audit found a launchd unit under `scripts/` with four home-directory paths and the account handle in its file name, a frontier sweep with 277 absolute paths, and two dhat profiles embedding the release-binary path. Outcome: `/experiments/` is untracked (PR #851, the archives live in `mununu-private/experiments/`), the launchd unit moved to `mununu-private/scripts/launchd/` and `/scripts/*.plist` is ignored.

## Rule 3 — unpublished write-ups

A blog post, paper outline, deck or tutorial lives in `mununu-private` until its public URL exists; the public repository then links the URL. [Claims Integrity](claims-integrity.md) applies at the moment of publication, so a draft that promises public replay material is checked against what the public tree actually carries before it ships. The top-level `publications/` moved to `mununu-private/publications/` on 2026-11-05. The per-example write-ups under `examples/industrial/*/publications/` are tracked on purpose: each is tied to a validated public example and carries its own four-gate checklist; whether they move is a separate decision.

## Rule 4 — live surfaces

A published artifact (claude.ai) is private by default and is never shared out; engagement and pitch material stays private. The status board's URL and this machine's nickname live in the gitignored local settings, never in a public repository, issue or PR. A local `mununu server` container binds loopback: `-p 127.0.0.1:8080:8080`, never a bare `-p 8080:8080`, which publishes the API on every interface of the host.

**Why.** The audit's own check found the tutorial-recording container published on `0.0.0.0:8080`, reachable on the LAN; it was recreated on loopback the same day.

## Rule 5 — scrubbing, and history

A name or path found on `main` is removed by a working-tree PR that replaces it with a generic phrase (the sentence stays true, only the party is dropped) plus a pass over the GitHub issue and PR text (`gh search issues`, `gh search prs`, the comments of each hit, edited in place). History is reported, never rewritten without explicit user instruction: a rewrite changes every later commit SHA (the briefings' provenance, the ledgers, `Repro:` trailers, release tags, Docker volumes keyed by commit), needs every clone and worktree re-cloned and every open PR rebased, needs GitHub Support to purge cached views and `refs/pull/*`, and does not reach forks. The decision and its cost are the user's; the agent prepares the inventory and the commit-map plan.

## Before opening a PR

- `git grep -n -i -E '<private-name list>'` over the tree returns nothing (the list is in `mununu-private/docs/security/private-names.txt`).
- `git grep -n -E '/Users/[a-z]|/home/[a-z]'` returns nothing.
- The commit message, the branch name and the PR body carry no names, no absolute paths and no board URL.
- A file that should not be public has its private copy and origin note before the commit that removes it.
