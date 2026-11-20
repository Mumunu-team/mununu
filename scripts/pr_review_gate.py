#!/usr/bin/env python3
"""The review gate: a sensitive pull request carries a review written on the other machine, and
a fix says which defect class it belongs to.

Policies: docs/policies/cross-host-review.md and docs/policies/fix-the-class.md.

  scripts/pr_review_gate.py --control        the negative controls: each pull request below that
                                             must be refused is refused, and each that must pass
                                             passes; else the gate is vacuous and this exits 1
  scripts/pr_review_gate.py --pr N           print the verdict for pull request N (changes nothing)
  scripts/pr_review_gate.py --pr N --post    also post it as the commit status `review-gate` on
                                             the head commit and keep the `needs-cross-review`
                                             label in step (what the workflow runs)

Both machines push from one GitHub account, so GitHub's own "required approving review" cannot
tell them apart. This gate reads two lines instead: `Authored-on: <machine>` in the pull request
body and `Cross-review: approve — host <machine>, head <sha>` in a comment. It is a forcing
function for sessions that follow the rules, not an access control.
"""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PATHS_FILE = ROOT / "scripts" / "review-sensitive-paths.txt"
CATALOG = "docs/defect-classes.md"
STATUS_CONTEXT = "review-gate"
LABEL = "needs-cross-review"
SENSITIVE_LABELS = {"soundness"}

FIX = re.compile(r"^fix[(:!]")
AUTHORED = re.compile(r"^Authored-on:[ \t]*([A-Za-z0-9][A-Za-z0-9_-]*)[ \t]*$", re.M)
REVIEW = re.compile(
    r"^Cross-review:[ \t]*(approve|changes-requested)[ \t]+[—-][ \t]+host[ \t]+(\S+),[ \t]+head[ \t]+([0-9a-f]{7,40})\b",
    re.M,
)
WAIVER = re.compile(
    r"^Cross-review:[ \t]*waived by the founder[ \t]+[—-][ \t]+(\S.*?),[ \t]+head[ \t]+([0-9a-f]{7,40})\b",
    re.M,
)
CLASS = re.compile(r"^Class:[ \t]*(.*\S)[ \t]*$", re.M)
CLASS_NONE = re.compile(r"^none[ \t]+[—-][ \t]+\S")
SLUG = re.compile(r"^[a-z0-9][a-z0-9-]*$")
CATALOG_HEADING = re.compile(r"^### `([a-z0-9][a-z0-9-]*)`", re.M)


def read_paths_file(text):
    """The prefixes and the line threshold from scripts/review-sensitive-paths.txt."""
    prefixes, threshold = [], 0
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("min-changed-lines:"):
            threshold = int(line.split(":", 1)[1])
        else:
            prefixes.append(line)
    return prefixes, threshold


def sensitive_lines(pr, prefixes):
    return sum(
        f.get("additions", 0) + f.get("deletions", 0)
        for f in pr["files"]
        if any(f["path"].startswith(p) for p in prefixes)
    )


def is_sensitive(pr, prefixes, threshold):
    """Why the pull request needs a cross-host review, or None."""
    labels = {label["name"] for label in pr.get("labels", [])}
    hit = sorted(labels & SENSITIVE_LABELS)
    if hit:
        return f"it carries the `{hit[0]}` label"
    changed = sensitive_lines(pr, prefixes)
    if changed >= threshold > 0:
        return f"it changes {changed} lines under the sensitive paths (threshold {threshold})"
    return None


def cross_review_failures(pr, why):
    head = pr["headRefOid"]
    author = AUTHORED.search(pr.get("body") or "")
    if not author:
        return [
            f"this pull request is sensitive ({why}) and its description has no `Authored-on: <machine>` "
            "line. Add one with the nickname of the machine that wrote it."
        ]
    author_host = author.group(1).lower()
    verdict = None  # the last review or waiver that names the current head wins
    for comment in pr.get("comments", []):
        body = comment.get("body") or ""
        for m in REVIEW.finditer(body):
            if head.startswith(m.group(3)):
                verdict = (m.group(1), m.group(2).lower())
        for m in WAIVER.finditer(body):
            if head.startswith(m.group(2)):
                verdict = ("waived", None)
    line = f"`Cross-review: approve — host <machine>, head {head[:8]}`"
    if verdict is None:
        return [
            f"this pull request is sensitive ({why}) and no review names its head commit {head[:8]}. "
            f"A session on the other machine reviews it and comments {line}; a review of an earlier "
            "commit does not count."
        ]
    kind, host = verdict
    if kind == "waived":
        return []
    if host == author_host:
        return [
            f"the review of {head[:8]} was written on `{host}`, the machine that authored the pull "
            "request. The review must come from the other machine."
        ]
    if kind == "changes-requested":
        return [f"the review of {head[:8]} on `{host}` asks for changes. Address them, then ask for {line}."]
    return []


def class_failures(pr, catalog_slugs):
    subjects = [pr.get("title") or ""] + [c.get("messageHeadline") or "" for c in pr.get("commits", [])]
    if not any(FIX.match(s) for s in subjects):
        return []
    declared = CLASS.findall(pr.get("body") or "")
    forms = "`Class: <slug>` (an entry of docs/defect-classes.md) or `Class: none — <why this is a single site>`"
    if not declared:
        return [f"this pull request fixes a defect and its description has no `Class:` line. Add one: {forms}."]
    if len(declared) > 1:
        return [f"the description has {len(declared)} `Class:` lines; a fix declares exactly one: {forms}."]
    value = declared[0].strip("`")
    if CLASS_NONE.match(value):
        return []
    if not SLUG.match(value):
        return [f"`Class: {value}` is neither a class slug nor `none — <why>`. Use {forms}."]
    if value not in catalog_slugs:
        known = ", ".join(sorted(catalog_slugs)) or "none yet"
        return [
            f"`Class: {value}` has no entry in docs/defect-classes.md at this pull request's head. "
            f"Add the entry in this pull request, or use an existing class: {known}."
        ]
    return []


def decide(pr, prefixes, threshold, catalog_slugs):
    """(failures, why-sensitive-or-None). An empty failure list is a pass."""
    why = is_sensitive(pr, prefixes, threshold)
    failures = class_failures(pr, catalog_slugs)
    if why:
        failures += cross_review_failures(pr, why)
    return failures, why


# --- the negative controls -------------------------------------------------------------------

HEAD = "a" * 40
BIG = [{"path": "crates/mununu-core/src/adapter/btor2/x.rs", "additions": 200, "deletions": 0}]


def _pr(**kw):
    base = {"title": "feat: x", "body": "", "headRefOid": HEAD, "labels": [], "files": [], "commits": [], "comments": []}
    base.update(kw)
    return base


def _review(text):
    return [{"body": text}]


CONTROLS = [
    # (name, pull request, must pass)
    ("a small docs change passes", _pr(files=[{"path": "docs/a.md", "additions": 5, "deletions": 1}]), True),
    ("a sensitive change with no Authored-on is refused", _pr(files=BIG), False),
    ("the template's unfilled Authored-on line is refused", _pr(files=BIG, body="Authored-on: <!-- nickname -->", comments=_review(f"Cross-review: approve — host laptop, head {HEAD[:8]}")), False),
    ("a sensitive change with no review is refused", _pr(files=BIG, body="Authored-on: box"), False),
    (
        "a review from the authoring machine is refused",
        _pr(files=BIG, body="Authored-on: box", comments=_review(f"Cross-review: approve — host BOX, head {HEAD[:8]}")),
        False,
    ),
    (
        "a review of an earlier commit is refused",
        _pr(files=BIG, body="Authored-on: box", comments=_review("Cross-review: approve — host laptop, head bbbbbbbb")),
        False,
    ),
    (
        "a review that asks for changes is refused",
        _pr(files=BIG, body="Authored-on: box", comments=_review(f"Cross-review: changes-requested — host laptop, head {HEAD[:8]}")),
        False,
    ),
    (
        "an approval from the other machine at the head passes",
        _pr(files=BIG, body="Authored-on: box", comments=_review(f"notes\nCross-review: approve — host laptop, head {HEAD[:8]}")),
        True,
    ),
    (
        "a later approval replaces an earlier request for changes",
        _pr(
            files=BIG,
            body="Authored-on: box",
            comments=_review(f"Cross-review: changes-requested — host laptop, head {HEAD[:8]}")
            + _review(f"Cross-review: approve - host laptop, head {HEAD[:8]}"),
        ),
        True,
    ),
    (
        "a founder waiver at the head passes",
        _pr(files=BIG, body="Authored-on: box", comments=_review(f"Cross-review: waived by the founder — the box is down, head {HEAD[:8]}")),
        True,
    ),
    ("the soundness label makes a small change sensitive", _pr(labels=[{"name": "soundness"}]), False),
    ("a fix with no Class line is refused", _pr(title="fix(x): y"), False),
    ("a fix commit under a feat title still needs a Class line", _pr(commits=[{"messageHeadline": "fix: y"}]), False),
    ("a fix naming an unknown class is refused", _pr(title="fix(x): y", body="Class: no-such-class"), False),
    ("a fix with two Class lines is refused", _pr(title="fix(x): y", body="Class: known-class\nClass: none — one site"), False),
    ("a fix naming a catalogued class passes", _pr(title="fix(x): y", body="Class: known-class"), True),
    ("a fix declared a single site, with its reason, passes", _pr(title="fix(x): y", body="Class: none — a typo in one message"), True),
    ("a bare `Class: none` is refused", _pr(title="fix(x): y", body="Class: none"), False),
]


def run_controls():
    prefixes, threshold = ["crates/mununu-core/src/adapter/btor2/"], 100
    bad = 0
    for name, pr, must_pass in CONTROLS:
        failures, _ = decide(pr, prefixes, threshold, {"known-class"})
        ok = (not failures) == must_pass
        print(f"{'ok  ' if ok else 'FAIL'} control: {name}")
        bad += not ok
    real_prefixes, real_threshold = read_paths_file(PATHS_FILE.read_text())
    if not real_prefixes or real_threshold <= 0:
        print(f"FAIL control: {PATHS_FILE.name} has no prefix or no `min-changed-lines:` — the gate would pass everything")
        bad += 1
    return bad


# --- the live run ----------------------------------------------------------------------------


def gh(*args, check=True):
    out = subprocess.run(["gh", *args], capture_output=True, text=True)
    if check and out.returncode != 0:
        sys.exit(f"gh {' '.join(args[:3])} failed: {out.stderr.strip()}")
    return out


def fetch(number):
    fields = "number,title,body,headRefOid,labels,files,commits,comments"
    pr = json.loads(gh("pr", "view", str(number), "--json", fields).stdout)
    raw = gh(
        "api", "-H", "Accept: application/vnd.github.raw",
        f"repos/{{owner}}/{{repo}}/contents/{CATALOG}?ref={pr['headRefOid']}", check=False,
    )
    slugs = set(CATALOG_HEADING.findall(raw.stdout)) if raw.returncode == 0 else set()
    return pr, slugs


def post(pr, failures, why):
    state = "failure" if failures else "success"
    if failures:
        text = failures[0]
    elif why:
        text = "cross-host review recorded at this head"
    else:
        text = "not sensitive; no cross-host review needed"
    gh(
        "api", "-X", "POST", f"repos/{{owner}}/{{repo}}/statuses/{pr['headRefOid']}",
        "-f", f"state={state}", "-f", f"context={STATUS_CONTEXT}", "-f", f"description={text[:139]}",
    )
    gh("label", "create", LABEL, "--description", "sensitive: waits for a review from the other machine",
       "--color", "D93F0B", check=False)
    waiting = bool(why) and any("review" in f.lower() or "Authored-on" in f for f in failures)
    gh("pr", "edit", str(pr["number"]), "--add-label" if waiting else "--remove-label", LABEL, check=False)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--control", action="store_true")
    ap.add_argument("--pr", type=int)
    ap.add_argument("--post", action="store_true")
    args = ap.parse_args()
    if args.control:
        bad = run_controls()
        print("controls:", "all hold" if not bad else f"{bad} did not hold")
        sys.exit(1 if bad else 0)
    if args.pr is None:
        ap.error("give --control or --pr N")
    prefixes, threshold = read_paths_file(PATHS_FILE.read_text())
    pr, slugs = fetch(args.pr)
    failures, why = decide(pr, prefixes, threshold, slugs)
    print(f"pull request #{pr['number']} at {pr['headRefOid'][:8]}: " + (f"sensitive — {why}" if why else "not sensitive"))
    for f in failures:
        print(f"  refused: {f}")
    if not failures:
        print("  passes")
    if args.post:
        post(pr, failures, why)
    # A posted run reports through the commit status; only a dry run exits non-zero on a refusal.
    sys.exit(0 if args.post or not failures else 1)


if __name__ == "__main__":
    main()
