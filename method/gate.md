# method/gate.md

Part of Kowloon, the method of `ALIGNMENT.md`. Read it before you work on signing or the gate. Neither is started except on the maintainer's explicit instruction.

## 10. Signing and the key registry

Not to be started by an agent. The maintainer creates these.

- **Intent is signed as content.** A manifest lists the hash of every file under `spec/`, of `ALIGNMENT.md`, `BINDINGS.md` and the files under `method/`, of `.gitattributes`, and of the gate script and the job that runs it. The maintainer signs the manifest with an SSH signing key that no agent session can read or use. `BINDINGS.md` says how the key is kept out of reach. Pre-flight (section 7) reports whether a session could sign; while it could, nothing signed with that key counts. The check does not depend on how a change was merged or on the hosting service, and an agent cannot produce the signature.
- **The key registry is `spec/signers`.** It is under `spec/`, where section 6 forbids agents to write, and it is covered by the manifest.
- **A registry in the change under review is never trusted.** The gate takes the registry from the last verified state, which is the tip of the protected branch, and verifies the new manifest against that. A change to the registry is valid only if the manifest that contains it verifies against the registry as it stood before.
- **The root of trust is outside the repository.** The first key's fingerprint is confirmed between people and pinned in two places no agent session can write: the hosting service's configuration for the required check, and the maintainer's own machine. The required check runs the gate as it is defined on the protected branch, not as the change defines it.
- A working tree cannot stop a process from writing a file. This arrangement makes such a write worthless: it cannot verify.
- Keys are rotated through the registry, by a change signed with a key already in it.

The trace map is deliberately not in the manifest; section 5 says why that is safe.

**What the maintainer sees before signing.** The gate prints what changed since the last signed manifest, per capability: files added under each pattern, files added unassigned, source lines, linked and unlinked tests, spec items, trace lines, document words, and experiments with their size. It also lists any file whose text changed with no version bump. There are no thresholds. The signature acknowledges the table, and states that the unbumped edits changed no meaning.

## 11. The gate

A single script, run locally and as a required check, verifies with text tools only:

- the manifest and its signature, as in section 10;
- that every tracked path has a class, every derived path has an intent, and every intent names a signed spec item;
- that every trace map line resolves, every `symbol` line only narrows (section 5), and every spec item with `checked-by: check` has a `check` line;
- that no link is stale;
- that each linked check passes, or, where the gate cannot run it (long, licensed, physical), has a recorded result naming its outcome as the tool reported it, the tool and version, the content hash of its paths and who ran it; a record with another outcome, or older than its paths, fails;
- that each linked check has a negative control that fails;
- that `constraint` items match their listings and budgets hold;
- that every assumption is named by a behaviour, and no hypothesis is past its date;
- that no linked check that passed in the baseline now fails;
- the change-of-intent conditions of section 8;
- that a closed area has no candidates, deltas or task lists left under `alignment/`;
- that `ALIGNMENT.md` and each file under `method/` is within its word cap, constants in the gate script set at the sizes of the signed version.

The script also reports every open finding whose question is overdue (section 4). An agent's part in enforcement is to run the script and read its exit code. In an area that is not closed the gate reports and never fails; in a closed area it fails. A gate that uses an external tool pins a released version of it.

**How the gate is provided.** In three layers, so that no project has its agent write its own checker.

- **Conformance fixtures define the gate.** A small sample repository holds one planted violation per rule above, with the result the gate must give. Any implementation giving those results conforms. The fixtures are how the gate itself is tested.
- **A reference gate.** One script with no build step, using only the minimum tools listed below. It covers only the rules that text tools and git decide, and is kept apart from any product the project ships, so that another repository can run it alone.
- **Project adapters.** Each project supplies two commands: one that lists its checks and one that runs a named check. Triage tooling, interface listings and budgets are also the project's.

An agent may draft the script and the fixtures; the maintainer signs them. The fixtures may be written before any area closes, because writing them shows which rules are ambiguous. The script is written when the first area is about to close, in this order: signature and registry, links and staleness, the table shown before signing.

**Release report.** Outside the gate, run at each release together with the blind re-extraction. It may use a pinned reachability tool. It lists only what is new since the previous report:

- spec items that no path and no check serves, `purpose` and `refusal` excepted;
- patterns in `.gitattributes` that match no file;
- code no capability reaches, and duplication;
- files added since the last signed manifest, by pattern;
- documents that nothing references, or that name a command or flag absent from the recorded interface listings;
- remote branches older than an age the maintainer sets;
- modules changed since the tutor last answered for them, as the list for the sampled conversation.

**Sampled, with the report.** A blind session that has not seen a check plants a violation from the item's sentence, in the code or in an agent's instructions and never on a physical rig. A check that still passes is a finding. A statistical check is scored over its full run count.

The maintainer signs the report once. A removal it names needs no further ruling. Worktrees, stashes and untracked files on a machine are that machine's owner's to clear and need no ruling.

**What each mechanism needs.** A rule belongs in the gate only if text tools or the project's own runner can decide it. What needs an approximate tool is a proposal for a ruling. What is costly is sampled or run on demand. What only a person can judge says so.

Two parts depend on something outside the repository and are to be confirmed: that the hosting service can run the required check as the protected branch defines it, and how a blind session is started.

**What a gate cannot show.** That the spec says what the maintainer meant, that the named person can in fact explain a module, or that a check tests what its spec item claims. Those rest on blind re-extraction of a closed area, run before a release or after a change of direction; on a sampled conversation with the tutor; and on the maintainer's reading of the divergences.

**The minimum, for any repository.** Files: `ALIGNMENT.md`, `BINDINGS.md` and `method/`, `spec/` with its manifest, signature and key registry, `.gitattributes`, the trace map, the open `alignment/` records, one gate script and one job that runs it. Tools: git, OpenSSH's signing, a hash tool, one script interpreter, and the project's own test runner. No database.
