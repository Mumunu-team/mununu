---
name: status-heartbeat
description: Write this session's row to the private status board — call it when a session starts, finishes, or blocks on a task. Reads the board URL and this machine's nickname from local settings; holds no URL, path, hostname, or name of its own.
---

# status-heartbeat

Record what this session is doing on the shared status board, so the board's `agents` table shows
which session runs what on which machine with a live heartbeat. Call it at three moments: when you
**start** a task, when you **finish or block** on one, and otherwise about every 20–30 minutes while
a long task runs.

The board URL and this machine's nickname are not written here — they live in the machine's
gitignored local settings. If either is unset, this machine has not been enrolled; say so and stop
(do not guess a URL or a nickname).

## Procedure

1. Read the board URL and nickname from the environment:
   ```sh
   echo "url=$MUNUNU_STATUS_BOARD_URL nick=$MUNUNU_MACHINE_NICK"
   ```
   If either is empty, stop and tell the user the machine is not enrolled (point them at the ops
   note that records how to set them).

2. Compose this session's row. Use the nickname for `machine`; never a hostname or an IP. A stable
   `doc_id` per session (the nickname plus a short suffix if several sessions run on one machine,
   e.g. `laptop-main`, `laptop-2`):
   - `session` — the doc id
   - `machine` — `$MUNUNU_MACHINE_NICK`
   - `task` — the task id or short label (match a `tasks` row where possible)
   - `branch`, `worktree` — the git branch and worktree directory basename (basename only)
   - `state` — one short phrase (`running: <what>`, `blocked: <what>`, `done: <what>`, `idle`)
   - `started_at` — keep the first value across heartbeats of the same session
   - `last_heartbeat` — now, UTC ISO (`date -u +%FT%TZ`)

3. Write it with the `ArtifactData` tool: `action: set` (or `update` to keep `started_at`),
   `url` the value from step 1, `collection: agents`, `doc_id` the session id, `data` the row.
   On an existing row pass `if_version` (the version from a prior read); re-read and retry if it
   is stale.

## Keep it clean

Nothing in a row but task ids, nicknames, branch and worktree basenames, and a short state phrase.
No absolute paths, no hostnames, no IPs, no customer, consumer, vendor, engagement or person names.
The board is private; keep its URL out of the public repos, issues, and PRs.
