# Fork transcripts in threads

Author: Claude Code (Claude Opus 5.5)

How forked subagents reach an `ai chat` thread at main 3f522d0c, measured on session 799603ff (thread `ai-chat`, Claude Code 2.1.302). Nothing was changed.

## What Claude Code writes

- Main transcript: `~/.claude/projects/<project>/<session>.jsonl` (25 MB, 7,333 lines).
- Companion directory `<session>/`: `subagents/agent-<id>.jsonl` plus `agent-<id>.meta.json` for each of 66 agents (53 MB), `tool-results/` (15 files), `custom-title.json`. `/tmp/claude-1000/<project>/<session>/tasks/<id>.output` are symlinks to the subagent transcripts.
- `meta.json`: `agentType` (`fork` 55, `general-purpose` 11), `isFork`, `description`, `toolUseId` (the parent's Agent call; absent for the 6 forks the user started by command, which carry `name` instead), `worktreePath`, `worktreeBranch`, `spawnDepth`, `finished`.
- A fork transcript opens with `{"type":"fork-context-ref", agentId, parentSessionId, parentLastUuid, contextLength}`: a pointer into the parent, not a copy of its context. Then sidechain entries: the parent's Agent `tool_use` replayed (parentUuid null), a user entry holding the `tool_result` "Fork started" and the directive as a text block, the fork's work, and its last `end_turn` text, which is its report. General-purpose subagents have no `fork-context-ref`.

In the main transcript:

- The Agent call's result is launch metadata: "Async agent launched successfully…agentId…output_file…" (`toolUseResult.status: async_launched`).
- The report comes back as a user entry with `origin.kind: "task-notification"`, `promptSource: "system"`, content `<task-notification>` with `task-id` (the agent id), `tool-use-id` (absent for command-started forks), `status` (`completed` or `failed`), `summary`, `result` (the fork's final text), `usage`, `worktree`. While the session is busy it comes as an `attachment` of type `queued_command`, `commandMode: "task-notification"`.
- Counts: 67 direct and 15 queued notifications (82, 9 of them `failed`); one agent can notify again after each `SendMessage` resume. 12 older hand-backs use `origin.kind: "peer"`.

## What ai chat keeps

- `sources/claude/<session>/` keeps every file above byte for byte (79 MB for `ai-chat`).
- `claude::subagents_from` (`src/chat/claude.rs:123`) parses each subagent transcript. The store writes `agents.jsonl` (66 records `{harness, agent, agent_type, description, call}`, `call` being the main item of the Agent call; null for the 6 command-started forks) and `agents/claude/<agent>.jsonl` (8,813 items, 14 MB; the main tree has 1,158).
- Nothing reads them back. `Snapshot::agents` and `agent_items` (`src/chat/store.rs:727`, `:762`) are used only by sync and tests. View, zoom, grep and rollup see main items only.
- Reports are dropped. `work()` (`src/chat/claude.rs:381`) takes `origin.kind` `peer` and `coordinator` only, so the 12 peer hand-backs are `Work` items (`agent general-purpose:`, as in turns 1 and 13–15), while all 82 task-notifications fail both `prompt()` and `queued()` and make no item.

## What zoom shows

`ai chat zoom 59.5+2 --tool-output`: the Agent call (description, prompt), then 59.6, the launch metadata, whose only pointer is the internal agent id. 59.7 and 59.8 are my replies to two reports that are not there.

## Effect on turns

The answer to a report continues the user's turn after its `end_turn`, so the turn's last `end_turn` text becomes its final reply. Turn 59's final reply is 59.8 (my answer to the merge report); 59.3, my answer to the user, counts as work. That hits the three-part summariser and the Recent level the view rework plans (message, work line, final reply).

## Ways to reach a fork's work

1. **Reports open turns.** Map task-notification user entries and queued attachments to `Kind::Work`, `from` the agent's description, `text` the result (status first when not `completed`), plus the agent id so the turn links to its agent. `ai/spec.md` already says a turn opens at a message from another agent; the peer hand-backs work this way. Cost: about 82 turns enter `ai-chat`, every later turn id shifts, and lines whose turns held reports go stale (spans and `[n.m]` citations); re-attaching rebuilds items, and lines need rebuilding, which is pending anyway for the new prompts.
2. **Nested ids for zoom.** A fork's items live under its call: `59.5/` lists the fork's turns, `59.5/1.12` is one item, `59.5/1.3+8` a range. The same turn rule splits the fork (directive, then each resume). Zoom of the call item and of the report turn ends with a hint (`fork "Three-part summariser": 201 items; zoom 59.5/`). Command-started forks anchor at the item made from `fork-context-ref.parentLastUuid`, which links the 6 that have no call. Nesting recurses for agents of agents (`spawnDepth` > 1).
3. **grep covers fork items**, printing nested ids. What a fork read and tried stays in its transcript; its report keeps only what it chose to say.
4. **No summary lines for fork items.** The report is the fork's own summary, written by the agent that did the work, and with option 1 it shows as a turn. Lines over 8,813 fork items, 7.6 times the main tree, would multiply summariser calls for text rarely read; zoom ranges and grep navigate it.

Rejected:
- Splicing fork items into the main tree as branches under the call: they would count in the view's branch line and could become "the newest" branch; the inherited context isn't in their files.
- Addressing forks by agent id (`--agent a1725…`): opaque, and Claude Code tells agents not to show those ids.

## Recommendation

1, then 2 and 3, then 4 by default. Option 1 changes turn boundaries, so it should land before the view rework fixes its levels and before `ai-chat` lines are rebuilt. 2 and 3 are independent of the view.

Decisions for the user:
- Reports as turns of their own, accepting renumbered turns.
- The `from` label: the agent's description or its type.
- The nesting separator `/`.
- Failed reports open turns too (fails visibly).

Out of scope: pi and Codex subagents; `auto-continuation` (4) and `plugin` (5) user entries, which the adapter also drops.
