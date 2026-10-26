#!/usr/bin/env python3
"""Issue trends over the GitHub tracker: fetch, classify, report, back-fill labels.

    scripts/issue_trends.py fetch   --repo Mumunu-team/mununu --out issues/mununu.json
    scripts/issue_trends.py report  issues/mununu.json [issues/rosf.json ...]
    scripts/issue_trends.py backfill issues/mununu.json --repo Mumunu-team/mununu [--apply]

`fetch` pulls every issue (title, body, state, labels, timeline) through `gh api graphql`.
`report` prints the tables the 2026-10-25 trend analysis was done by hand: issues per
month per class, per origin, reopen events, duplicate candidates, fix chains.
`backfill` proposes one class label and one `found:*` label per issue — `--override FILE`
(a reviewer's corrections) first, then the form's dropdowns when the issue was filed through `.github/ISSUE_TEMPLATE/defect.yml`, keyword
heuristics otherwise — and, with `--apply`, adds the labels an issue lacks through
`gh issue edit`. Without `--apply` it only prints the proposal. It never removes a label.

The classes and the origins are the labels `docs/policies/cross-repo-impact.md` names;
the heuristics are a first reading, not a verdict — read the dry run before applying.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path

CLASSES = [
    "soundness",
    "silent",
    "crash",
    "flake",
    "decidability",
    "coverage",
    "semantics-docs",
    "performance",
    "ergonomics",
]
ORIGINS = ["internal", "monono", "opentitan", "hwmcc", "rosf"]

# Priority order: the first class whose pattern matches wins. A wrong verdict outranks a
# swallowed error outranks a panic; the rest are reached only when none of those apply.
CLASS_PATTERNS = [
    ("soundness", r"unsound|wrong verdict|false[- ](holds?|violated|positive|negative)|spurious(ly)? (holds?|violated)|incorrect(ly)? (verdict|holds?|violated)|should (hold|be violated|have (held|been violated))|holds? (on|for) .{0,40}(violat|bug|fault)"),
    ("silent", r"silent|swallow|dropped|ignored|no-?op|never (applied|reached|read|consulted)|vacuous|not applied|withheld|without (a |any )?(note|reason|warning)|discard"),
    ("crash", r"panic|overflow|SIGSEGV|segfault|abort|unwrap|out of memory|\bOOM\b|exit (code )?101|stack overflow|arena|unreachable!|index out of"),
    ("flake", r"flak|intermittent|host-dependent|nondeterministic|sometimes|CI (is )?red|26%"),
    ("decidability", r"⊥|\bunknown\b|abstain|budget|timeout|bit cap|\bcone\b|undecided|does not (decide|converge)|too (slow|large) to decide|inconclusive"),
    ("coverage", r"unsupported|not supported|cannot (translate|lift|parse)|fragment|lift (fail|wall|does)|does not parse|system task|not (yet )?implemented|missing (support|arm)"),
    ("semantics-docs", r"document(ed|ation|s say)|docs? (say|claim|state)|wiki|README|semantics|disagree|contradict|misleading"),
    ("performance", r"\bslow\b|minutes|hours|regression|wall[- ]clock|memory usage|\d+ ?(s|min)\b.*(took|takes)"),
    ("ergonomics", r"flag|\bCLI\b|\bAPI\b|JSON|rename|message|report field|surface|exit code|parity|ergonomic"),
]
ORIGIN_PATTERNS = [
    ("monono", r"\bmonono\b"),
    ("opentitan", r"opentitan|\bedn\b|csrng|keymgr|rom_ctrl|prim_|otbn|pattgen|lowrisc"),
    ("hwmcc", r"hwmcc|am2901|aiger|btor2 benchmark"),
    ("rosf", r"\brosf\b"),
]

FORM_FIELD = re.compile(r"^### (Class|Found)\s*\n+\s*([a-z-]+)\s*$", re.MULTILINE)

GRAPHQL = """query($owner:String!,$name:String!,$cursor:String){
  repository(owner:$owner,name:$name){
    issues(first:50,after:$cursor,orderBy:{field:CREATED_AT,direction:ASC}){
      pageInfo{hasNextPage endCursor}
      nodes{number title state stateReason createdAt closedAt author{login}
        labels(first:20){nodes{name}} comments{totalCount} body
        timelineItems(first:50,itemTypes:[REOPENED_EVENT,CLOSED_EVENT,CROSS_REFERENCED_EVENT]){
          nodes{__typename
            ... on ReopenedEvent{createdAt}
            ... on ClosedEvent{createdAt closer{__typename ... on PullRequest{number} ... on Commit{oid}}}
            ... on CrossReferencedEvent{source{__typename ... on PullRequest{number state} ... on Issue{number}}}}}}}}}"""


def gh(args: list[str]) -> str:
    return subprocess.run(["gh", *args], capture_output=True, text=True, check=True).stdout


def fetch(repo: str) -> list[dict]:
    owner, name = repo.split("/", 1)
    out: list[dict] = []
    cursor = None
    while True:
        args = ["api", "graphql", "-f", f"query={GRAPHQL}", "-F", f"owner={owner}", "-F", f"name={name}"]
        if cursor:
            args += ["-F", f"cursor={cursor}"]
        page = json.loads(gh(args))["data"]["repository"]["issues"]
        out.extend(page["nodes"])
        if not page["pageInfo"]["hasNextPage"]:
            break
        cursor = page["pageInfo"]["endCursor"]
    return out


def labels_of(issue: dict) -> set[str]:
    return {label["name"] for label in issue.get("labels", {}).get("nodes", [])}


def classify(issue: dict) -> tuple[str, str, str]:
    """(class, origin, how) — the form's dropdowns when present, else the heuristics."""
    text = f"{issue.get('title', '')}\n{issue.get('body') or ''}"
    form = {k.lower(): v for k, v in FORM_FIELD.findall(text)}
    cls = form.get("class") if form.get("class") in CLASSES else None
    origin = form.get("found") if form.get("found") in ORIGINS else None
    how = "form" if cls and origin else "heuristic"
    if cls is None:
        cls = next((c for c, pat in CLASS_PATTERNS if re.search(pat, text, re.IGNORECASE)), "ergonomics")
    if origin is None:
        origin = next((o for o, pat in ORIGIN_PATTERNS if re.search(pat, text, re.IGNORECASE)), "internal")
    return cls, origin, how


def month(ts: str | None) -> str:
    return (ts or "")[:7] or "?"


def effective_class(issue: dict) -> tuple[str, str]:
    """The labels when the issue carries them, else the classification."""
    have = labels_of(issue)
    cls = next((c for c in CLASSES if c in have), None)
    origin = next((o for o in ORIGINS if f"found:{o}" in have), None)
    if cls is None or origin is None:
        c2, o2, _ = classify(issue)
        cls, origin = cls or c2, origin or o2
    return cls, origin


def report(files: list[Path]) -> None:
    for path in files:
        issues = json.loads(path.read_text())
        print(f"## {path.stem} — {len(issues)} issues, {sum(i['state'] == 'OPEN' for i in issues)} open\n")
        by_month: dict[str, Counter] = defaultdict(Counter)
        by_origin: Counter = Counter()
        by_class: Counter = Counter()
        for i in issues:
            cls, origin = effective_class(i)
            by_month[month(i["createdAt"])][cls] += 1
            by_origin[origin] += 1
            by_class[cls] += 1
        cols = [c for c in CLASSES if by_class[c]]
        print("| month | " + " | ".join(cols) + " | total |")
        print("|---|" + "---|" * (len(cols) + 1))
        for m in sorted(by_month):
            row = by_month[m]
            print(f"| {m} | " + " | ".join(str(row[c]) for c in cols) + f" | {sum(row.values())} |")
        print("\n**By class:** " + ", ".join(f"{c} {n}" for c, n in by_class.most_common()))
        print("**By origin:** " + ", ".join(f"{o} {n}" for o, n in by_origin.most_common()) + "\n")

        reopened = [i for i in issues if any(t["__typename"] == "ReopenedEvent" for t in i["timelineItems"]["nodes"])]
        print(f"**Reopened ({len(reopened)}):** " + (", ".join(f"#{i['number']}" for i in reopened) or "none") + "\n")

        dupes = [i for i in issues if i.get("stateReason") == "DUPLICATE" or re.search(r"\bduplicate of #\d+", i.get("body") or "", re.I)]
        print(f"**Closed or marked as duplicates ({len(dupes)}):** " + (", ".join(f"#{i['number']}" for i in dupes) or "none"))
        words = lambda s: set(re.findall(r"[a-z0-9_]{4,}", s.lower()))  # noqa: E731
        cands = []
        for a_idx, a in enumerate(issues):
            for b in issues[a_idx + 1 :]:
                wa, wb = words(a["title"]), words(b["title"])
                if wa and wb and len(wa & wb) / len(wa | wb) >= 0.6:
                    cands.append((a["number"], b["number"]))
        print(f"**Title near-duplicates (Jaccard ≥ 0.6, {len(cands)}):** " + (", ".join(f"#{a}/#{b}" for a, b in cands[:40]) or "none") + "\n")

        # A chain: an issue whose body cites an earlier issue of the same class.
        by_number = {i["number"]: i for i in issues}
        chains: dict[int, list[int]] = defaultdict(list)
        for i in issues:
            cls, _ = effective_class(i)
            for ref in {int(n) for n in re.findall(r"#(\d+)", i.get("body") or "")}:
                if ref in by_number and ref < i["number"] and effective_class(by_number[ref])[0] == cls:
                    chains[ref].append(i["number"])
        long = {k: v for k, v in chains.items() if len(v) >= 2}
        print(f"**Chains (an issue cited by ≥ 2 later issues of its class, {len(long)}):**")
        for root, members in sorted(long.items()):
            print(f"- #{root} ({effective_class(by_number[root])[0]}) ← " + ", ".join(f"#{n}" for n in sorted(members)))
        print()


def backfill(path: Path, repo: str, apply: bool, overrides: dict[int, dict] | None = None) -> None:
    issues = json.loads(path.read_text())
    overrides = overrides or {}
    rows = []
    for i in issues:
        have = labels_of(issue=i)
        cls, origin, how = classify(i)
        if (fix := overrides.get(i["number"])) is not None:
            cls, origin, how = fix.get("class", cls), fix.get("found", origin), "override"
        # In a repository's own tracker its own name is not an origin.
        if origin == repo.rsplit("/", 1)[-1]:
            origin = "internal"
        add = [l for l in (cls, f"found:{origin}") if l not in have]
        if any(c in have for c in CLASSES):
            add = [l for l in add if l not in CLASSES]
        if any(l.startswith("found:") for l in have):
            add = [l for l in add if not l.startswith("found:")]
        rows.append((i["number"], i["state"], how, cls, origin, add, i["title"]))
    print("| # | state | by | class | found | adds | title |")
    print("|---|---|---|---|---|---|---|")
    for n, state, how, cls, origin, add, title in rows:
        print(f"| #{n} | {state.lower()} | {how} | {cls} | {origin} | {','.join(add) or '—'} | {title[:70]} |")
    todo = [(n, add) for n, _, _, _, _, add, _ in rows if add]
    print(f"\n{len(todo)} issues would gain labels; {len(rows) - len(todo)} already carry both.")
    if not apply:
        print("dry run — pass --apply to add them (labels are only ever added, never removed).")
        return
    for n, add in todo:
        gh(["issue", "edit", str(n), "--repo", repo, "--add-label", ",".join(add)])
        print(f"#{n} += {','.join(add)}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch", help="pull every issue of a repository into a JSON file")
    f.add_argument("--repo", required=True)
    f.add_argument("--out", required=True, type=Path)
    r = sub.add_parser("report", help="trend tables over fetched JSON files")
    r.add_argument("files", nargs="+", type=Path)
    b = sub.add_parser("backfill", help="propose (and with --apply add) class + found:* labels")
    b.add_argument("file", type=Path)
    b.add_argument("--repo", required=True)
    b.add_argument("--apply", action="store_true")
    b.add_argument("--override", type=Path, help='JSON {"<number>": {"class": ..., "found": ...}} read by a reviewer, applied over the heuristics')
    args = ap.parse_args()
    if args.cmd == "fetch":
        data = fetch(args.repo)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(data, indent=1))
        print(f"{args.repo}: {len(data)} issues → {args.out}")
    elif args.cmd == "report":
        report(args.files)
    else:
        overrides = {int(k): v for k, v in json.loads(args.override.read_text()).items()} if args.override else None
        backfill(args.file, args.repo, args.apply, overrides)
    return 0


if __name__ == "__main__":
    sys.exit(main())
