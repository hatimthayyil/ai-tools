# pi and Codex session logs, for `ai chat`

Sources and versions:

- pi source: `/hatimthayyil/code/pi` at `19451accd` (2026-09-20), package version 0.86.1 (`packages/coding-agent/package.json`). Installed pi is 1.1.0 (`/nix/store/6bnb2h1llhj0vy86x8f7krmkpwf8acrd-pi-1.1.0/libexec/pi`, below `$PI110`). Where they differ, 1.1.0 docs are cited.
- pi paths below are relative to `pi/packages/coding-agent/`.
- Codex source: `/hatimthayyil/code/codex` at `a6c5f3a9fa` (2026-10-05); installed `codex-cli 0.162.1`. Codex paths below are relative to `codex/codex-rs/`.
- Real logs: 55 pi files (`~/.pi/agent/sessions`), 78 Codex rollouts (`~/.codex/sessions`), inspected with `jq`.

## pi

### 1. Session file format

Verified:

- One JSONL file per session: `~/.pi/agent/sessions/--<cwd with / → ->--/<timestamp>_<uuid>.jsonl` (`docs/session-format.md:7-11`). Header line `{"type":"session","version":3,"id","timestamp","cwd","parentSession"?}`, outside the tree (`session-format.md:212-224`).
- All other entries carry `type, id (8 hex), parentId, timestamp` (`session-format.md:199-208`). Types: `message`, `model_change`, `thinking_level_change`, `usage`, `compaction`, `branch_summary`, `custom` (extension state, not in context), `custom_message` (extension text, in context), `label`, `session_info` (`session-format.md:210-341`). 1.1.0 adds `context_edit {targetId, replacement}`: an append-only edit that omits (`null`) or replaces an earlier entry's content in future model context only (`$PI110/docs/session-format.md:139-148`).
- `message.message` is an `AgentMessage`. Roles: `system`, `user`, `assistant`, `toolResult`, `bashExecution`, `custom`; `branchSummary` and `compactionSummary` exist only in memory (`session-format.md:77-195`).
- Tool calls are `{"type":"toolCall",id,name,arguments}` blocks inside the assistant message's `content`. Each result is its own `message` entry with `role:"toolResult", toolCallId, toolName, content, isError`, chained by `parentId` after the assistant entry (`session-format.md:67-74,113-122,238-240`).
- System messages persist the prompt and tool set as diffs (`sections`, `toolsAdded/Removed`) (`session-format.md:228-233`).
- Persistence: every `message_end` for system/user/assistant/toolResult is appended (`src/core/agent-session.ts:707-724`). An assistant message containing tool calls therefore reaches disk before its tools run. The file is first written only once an assistant message exists, then appended per entry (`src/core/session-manager.ts:1071-1097`).
- Compaction entry: `{type:"compaction", summary, firstKeptEntryId, tokensBefore, details?, fromHook?, usage?, systemMessage?}`. Context rebuild uses the latest compaction on the path, plus entries from `firstKeptEntryId`, plus later entries (`session-format.md:269-283,360-379`). Raw pre-compaction entries stay in the file.
- Branch summary entry: `{type:"branch_summary", parentId:<branch point>, fromId:<abandoned leaf>, summary, details?, fromHook?}` (`session-format.md:285-298`).
- Label entry: `{type:"label", targetId, label}`, where `label: undefined` clears it. It is itself a tree node (`session-format.md:323-331`; label resolution `session-manager.ts:1022-1030`).
- Real data: 4180 `message` (1822 assistant, 2103 toolResult, 240 user, 14 system), 15 `compaction`, 1 `branch_summary`, 6 `context_edit`, 349 `custom`, 41 `custom_message` (jq over all files). 13 of 15 compactions have `fromHook:true` with pi-blackhole `details` (`compactor`, `om.folded`, `sections` …); 2 are native (`details:{readFiles,modifiedFiles}`). Extensions also put foreign JSONL under `sessions/<dir>/subagent-artifacts/` (records with no `type`). Skip them.

### 2. Tree, fork, branching, rewind

Verified:

- **Active path:** on load, the leaf is the **last entry in the file** (`session-manager.ts:1014-1022`). The path walks `parentId` from the leaf to the root (`session-manager.ts:356-380`). `branch(id)` only moves the in-memory leaf and writes nothing; the next append becomes a child of it (`session-manager.ts:1436-1450`). `branchWithSummary` appends a `branch_summary` under the target (`session-manager.ts:1459-1486`).
- **`/tree`:** navigates in place within one file. Selecting a user message moves the leaf to its parent and puts the text in the editor for resubmission. The optional summary of the abandoned branch is an LLM call that walks from the old leaf to the common ancestor (`docs/sessions.md:70-141`, `docs/compaction.md:150-215`).
- **`/fork`, `/clone`:** write a new file whose header carries `parentSession` (an absolute path) and contains the copied path, with no summary (`sessions.md:119-128`, `session-format.md:220-224`, `createBranchedSession` `session-manager.ts:1489+`).
- Branching is common in practice. 10+ real files have entries with several children, mostly sibling `user` messages (edit-and-resubmit) and one `user` plus `branch_summary` pair (jq over `parentId` duplicates). No real file has `parentSession`.
- **pi-rewind:** files are git snapshots under `refs/pi-checkpoints/…` (`pi-rewind/src/core.ts:19,647`). Rewinding the conversation calls `ctx.navigateTree(target, {summarize:true})` (`pi-rewind/src/commands.ts:141-156`), so it shows up in the log as an ordinary `/tree` branch plus a `branch_summary`. The prateekmedia checkpoint extension works the same way (`prateekmedia.pi-hooks/checkpoint/checkpoint.ts:441-476`).

Inference:

- A CLI invoked by the agent from pi's bash tool always runs after the assistant entry holding that tool call was appended, so "walk `parentId` from the last line" yields the live active path.
- The only stale case is a `/tree` move with nothing appended yet, which cannot happen while a tool is running.

### 3. Extension API

Verified:

- **`session_before_compact`:** receives `{preparation:{messagesToSummarize, turnPrefixMessages, previousSummary, fileOps, tokensBefore, firstKeptEntryId, settings}, branchEntries, customInstructions, reason: manual|threshold|overflow, willRetry, signal}`. It returns `{cancel:true}` or `{compaction:{summary, firstKeptEntryId, tokensBefore, details?, usage?}}` (`docs/compaction.md:277-312`, `src/core/extensions/types.ts:1180-1183`). It fires for `/compact`, threshold and overflow alike. An extension-supplied summary is stored with `fromHook:true`.
- **`session_compact` / `session_compact_failed`:** notify after the fact (`docs/extensions.md:452-491`).
- **`session_before_tree`:** can cancel or supply a branch summary (`compaction.md:366-396`).
- **`context`:** fires before each LLM call and returns `{messages}` to rewrite context non-destructively (`extensions.md:678-688`, `types.ts:689-692,1121-1123`).
- **`message_end`:** can replace the finalized message, keeping its role (`extensions.md:618-652`).
- **`before_agent_start`:** can inject a persistent `custom_message` and edit the system prompt (`extensions.md:530-566`).
- **`pi.sendMessage(..., {deliverAs: steer|followUp|nextTurn, triggerTurn})`:** injects a context message (`extensions.md:1455-1476`).
- **`pi.registerCommand`, `pi.registerTool`, `pi.appendEntry`:** `extensions.md:1404,1510,1564`.
- **pi 1.1.0 additions:** `turn_end` and `agent_before_settle` can append `custom`, `custom_message`, `context_edit` or `compaction` entries and request a continuation (`$PI110/docs/extensions.md:117`).
- **Session file, extension side:** `ctx.sessionManager.getSessionFile()`, `getBranch()`, `getLeafId()` (`extensions.md:1024-1035`; `session-format.md:470-480`).
- **Session file, CLI side:** pi's bash tool exports `PI_SESSION_ID`, `PI_SESSION_FILE` (absolute path, unset when ephemeral), `PI_PROVIDER`, `PI_MODEL`, `PI_REASONING_LEVEL` to every command (`src/core/tools/bash.ts:177-192`, on by default `bash.ts:234`; `docs/environment-variables.md:7,26-30`; same in 1.1.0).
- **pi-blackhole:** it owns `session_before_compact` (`pi-blackhole/src/hooks/before-compact.ts:302`). Its summary is deterministic, built without an LLM: regex section extraction (goal, files, commits, preferences, brief transcript) via a `compile()` pipeline. It returns `{compaction:{summary, details, tokensBefore, firstKeptEntryId}}` with its own cut point (`before-compact.ts:286-290,816-828`) and returns `{cancel:true}` when it declines (`before-compact.ts:381,476`). Three background LLM workers (observer, reflector, dropper) record observations and reflections as `custom` entries (`om.*`). Those entries are folded into the summary. A `context` hook replays its projection on each call (`src/hooks/compaction-context.ts:12-40`), and a `recall` tool does BM25 or regex search over the raw session file, defaulting to the active lineage (`getBranch()`, `src/core/lineage.ts:10-27`). Its `/blackhole` command triggers compaction with a marker instruction. See `pi-blackhole/README.md`, "Who owns the compaction?" and "How it works".
- **pi-claude-bridge:** another `session_before_compact` owner that runs its own `compact()` and cancels on failure (`pi-claude-bridge/src/index.ts:2170-2200`). Compaction ownership is first-come and already contested.

## Codex

### 4. Rollout format, compaction, resume/fork, hooks

Verified:

- **File:** `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`. Cold files may be compressed to `.jsonl.zst` (`rollout/src/compression.rs:29,51-101`). Each line is `{timestamp, ordinal?, type, payload}` (`history/src/lib.rs:356-367`). `RolloutItem` variants are `session_meta`, `response_item`, `inter_agent_communication(_metadata)`, `compacted`, `turn_context`, `token_usage_record`, `world_state`, `security_risk_score`, `retained_context`, `event_msg`, `realtime_item` (`history/src/lib.rs:210-226`).
- **Real counts:** 5353 `response_item` (883 `message`, 1064 `reasoning`, 444 `function_call` + 444 `function_call_output`, 1109 `custom_tool_call` + 1108 output, 3 `web_search_call`). 5683 `event_msg` (3361 `item_completed`, 1741 `token_count`, 189 `task_started`, 125 `task_complete`, 25 `turn_aborted`, 13 `user_message`, 2 `thread_rolled_back`). 3 `compacted`, 116 `session_meta`.
- **Messages:** `response_item.message` has `role` `developer|user|assistant`. Assistant messages carry `phase` (`commentary`/final). Harness context is injected as `role:user` items (AGENTS.md, `<skill>`, environment), so real user turns should come from `event_msg.item_completed` with `item.type=="UserMessage"` (current) or `event_msg.user_message` (older). `AgentMessage`, `Reasoning`, `CommandExecution`, `FileChange`, `McpToolCall` and `ContextCompaction` items are also present (jq on `rollout-2026-10-11T13-05-43-…`).
- **Tool calls:** `function_call{name, arguments, call_id}` → `function_call_output{call_id, output}`, or `custom_tool_call{name, input, call_id}` → `custom_tool_call_output`, paired by `call_id`.
- **Compaction (`/compact` and auto):** both append a `compacted` item. `CompactedItem{message, replacement_history, retained_context, window_number, first/previous/window_id, compaction_response_id, …}` (`history/src/lib.rs:286-306`). The choice is per provider (`core/src/tasks/compact.rs:59-90`):
  - `RemoteCompactionSupport::V2` (OpenAI) runs server-side.
  - Otherwise local compaction uses `compact_prompt` or the built-in prompt. The summary becomes `SUMMARY_PREFIX + model text` (`core/src/compact.rs:370-385`), and the recent user messages are kept, capped at 20k tokens (`compact.rs:63,673-700`).
  - All 3 real compactions are remote. `message` is empty, and `replacement_history` = retained user/developer messages plus one `{"type":"compaction","encrypted_content":…}` item (6.5 KB opaque). The summary text is not readable.
  - Pre-compaction items stay in the file (ordinals before `compacted` remain).
- **Rollback and revert:** rollback appends `event_msg.thread_rolled_back{num_turns}`. Replay drops that many trailing user turns (`core/src/session/rollout_reconstruction.rs:259-262`, `core/src/thread_rollout_truncation.rs:41-60`). `thread/revert` exists (`app-server-protocol/src/protocol/common.rs:766`), and a reverted thread's file name carries a new rollout id (`protocol/src/protocol.rs:3103-3113`).
- **Fork:** the new rollout's `session_meta` has `forked_from_id`.
  - Legacy history mode copies the parent's history inline: a real forked subagent rollout has its own `session_meta` at ordinal 0 and the parent's at ordinal 1 (`rollout-2026-10-07T07-39-17-…`).
  - Paginated mode (`session_meta.history_mode`, `history_base`) references the parent rollout prefix by ordinal and byte offset instead of copying (`protocol/src/protocol.rs:3123-3200`, `thread-store/src/local/rollout_lineage.rs:14-30`).
- **Resume:** appends to the same rollout.
- **Index:** `~/.codex/state_5.sqlite` table `threads(id, rollout_path, cwd, source, …)` and `session_index.jsonl` (`id, thread_name`).
- **Env vars:** shell commands get `CODEX_THREAD_ID` (always, even with `include_only`), `CODEX_SESSION_ID` (root thread id) and `CODEX_VERSION` (`core/src/exec_env.rs:30-50`, `protocol/src/shell_environment.rs:6-7,153`). No variable gives the rollout path.
- **Hooks:** Codex runs a Claude-compatible hook engine (`hooks/src/events/session_start.rs:13`). Events: `PreToolUse, PermissionRequest, PostToolUse, PreCompact, PostCompact, SessionStart, SessionEnd, UserPromptSubmit, SubagentStart, SubagentStop, Stop, Interrupt` (`app-server-protocol/src/protocol/v2/hook.rs:19-21`). Each input carries `session_id`, `transcript_path` (the rollout path, `core/src/session/mod.rs:5005-5017`) and `cwd`.
  - `PreCompact` and `PostCompact` take `trigger` and can only stop. `decision:block` is rejected (`hooks/src/events/compact.rs:23-56,406-425`). A PreCompact stop aborts the turn (`core/src/compact_remote_v2.rs:153-167`).
  - After every compaction Codex queues `SessionStart` with `source:"compact"` (`core/src/session/mod.rs:4097-4100`). Its `hookSpecificOutput.additionalContext` is injected into the model context (`hooks/src/schema.rs:388-403`, `core/src/hook_runtime.rs:126-175`). `UserPromptSubmit` also injects `additionalContext`.
  - The user already runs `ai memory hook` on `UserPromptSubmit` (`~/.codex/hooks.json`).
  - Legacy `notify` runs a program after each turn with `{thread-id, turn-id, cwd, input-messages, last-assistant-message}` (`hooks/src/legacy_notify.rs:13-40`).
- **External summary:** no hook or config lets an external tool supply the compaction summary. `compact_prompt` / `experimental_compact_prompt_file` only change the prompt of local compaction (`config/src/config_toml.rs:274-275,557`; `tasks/compact.rs:78-85`).

Inference:

- Rollouts are written through an async recorder with deferred creation (`rollout/src/recorder.rs:972-986,1051,1791-1812`). Real rollouts show the `exec` call items before their outputs. The in-flight call that launched the CLI may or may not be on disk yet; a reader must tolerate a truncated last line.

## 5. Integration matrix

| | pi | Codex |
|---|---|---|
| (a) find the log from a shell CLI | `$PI_SESSION_FILE` (verified) | `$CODEX_THREAD_ID` → `state_5.sqlite threads.rollout_path`, or glob `sessions/**/rollout-*-<id>.jsonl[.zst]`. The glob can miss after a revert. Cleaner: a `SessionStart` hook records `transcript_path` keyed by `session_id`. |
| (b) replace or augment compaction | Replace: `session_before_compact` returns our summary (an extension shelling out to `ai chat`). Conflicts with pi-blackhole and pi-claude-bridge, since only one owner works. Augment without owning: `session_compact`, then `pi.sendMessage`. | Cannot replace. Native summary is opaque for OpenAI. Augment: `SessionStart{source:"compact"}` hook prints `additionalContext`. `PreCompact` can only abort. |
| (c) inject resume context | `before_agent_start` message, `sendMessage`, `context` hook, or the agent runs `ai chat` and reads stdout | `SessionStart` (`startup`/`resume`/`compact`) or `UserPromptSubmit` `additionalContext`, or the agent runs `ai chat` |

## Branching: recommendation

The options:

- **(i) Active path only.** pi gives the path directly: walk `parentId` from the last line. Codex gives it by applying `thread_rolled_back` markers; forks are separate files. On each sync, diff the active path against the ids already recorded and truncate to the longest common prefix, then append. With binary merging, truncating the tail invalidates only the O(log n) nodes that cover it. Branch summaries (`branch_summary`, also how pi-rewind shows up) sit on the active path and roll up as ordinary content. Abandoned branches survive in the source log and in our git history (the commit before truncation).
- **(ii) Forks in our tree.** Needs our own id/parent graph and branch naming, and the merge tree is no longer one sequence per thread. Codex has no in-file branches to feed it.
- **(iii) Unsupported.** Wrong in pi: edit-and-resubmit branches are common in real logs, and ignoring them would merge dead and live turns.

Use (i). It is the simplest correct choice: both formats define a single live path, and truncate-then-append handles `/tree`, rewind and rollback with one mechanism. Treat `/fork` and `/clone` (pi `parentSession`) and Codex `forked_from_id` as a new session that may start a new named thread from the parent's prefix. Defer that until needed.

## Implications for ai chat

- Source adapters, per format:
  - pi: entries → active path → pairs. Skip `custom`, `label`, `usage`, `model_change` and `thinking_level_change`. Treat `custom_message`/`branch_summary` as context. Collapse `toolCall`/`toolResult` into the assistant turn.
  - Codex: `UserMessage` items define turn starts. Collapse `function_call`/`custom_tool_call`/`reasoning` into the turn. Apply `thread_rolled_back`. Skip `developer` and injected `role:user` items.
- Key every pair by its source ids (pi entry id, Codex `turn_id`/ordinal) so re-sync is a prefix diff.
- Never depend on native compaction content. Both logs keep the full raw history, and the Codex summary is encrypted. Treat `compaction`/`compacted` entries as markers only.
- Session discovery: pi `$PI_SESSION_FILE`; Codex `$CODEX_THREAD_ID` plus a lookup. Detect the harness from env (`PI_CODING_AGENT=true`, `CODEX_THREAD_ID`).
- Resume-after-compaction:
  - Codex: a `SessionStart` command hook (matcher `compact`) running `ai chat …` that emits `additionalContext`. This is the same hook shape as Claude Code, so one code path serves both.
  - pi: a thin extension. It either owns `session_before_compact` (returns our rolled-up view as `summary`, `fromHook:true`) or, when another compactor is installed, injects after `session_compact` via `sendMessage`. The ownership choice belongs in pi config, not ai chat.
- Read-only and tolerant: accept a partial last line, `.zst` rollouts, unknown entry types (pi 1.1.0 already added `context_edit`; local pi source lags installed pi), and foreign JSONL in pi session dirs.
