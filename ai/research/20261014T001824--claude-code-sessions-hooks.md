# Claude Code sessions, compaction and hooks: integration surface for `ai chat`

Claude Code v2.1.296 (the running binary). Sources:
- Docs: code.claude.com/docs/en/{hooks, hooks-guide, sessions, checkpointing, context-window, how-claude-code-works, env-vars, model-config, skills, claude-directory, prompt-caching}, fetched 2026-10-14.
- Logs: `~/.claude/projects/*` transcripts, versions 2.1.220 to 2.1.296, inspected read-only with jq. Only their structure is quoted here.
- An earlier hook experiment on this machine (v2.1.283). Its script is `~/.cache/cc-blackhole-feasibility/hooks/hook.sh`, its hook input log is `.../hooks/logs/all.jsonl`, and its transcripts are under `~/.claude/projects/-home-hatim--cache-cc-blackhole-feasibility-hooks-proj/`.
- Prior notes: `/hatimthayyil/code/claude-code-tree/ai/research/feasibility.md` (tree structure, forks) and the VCC parser at `/hatimthayyil/code/VCC/skills/conversation-compiler/scripts/VCC.py`.
- `claude-agent-sdk-typescript` holds only the CHANGELOG and examples, no CLI source.

Labels:
- **[doc]**: official docs.
- **[obs]**: observed in logs.
- **[exp]**: observed in the hook experiment.
- **[inf]**: inference.

## 1. Session JSONL format

**Status.** The format is internal and unstable. "The entry format is internal to Claude Code and changes between versions, so scripts that parse these files directly can break on any release" [doc sessions#where-transcripts-are-stored]. The supported readers are `/export`, `claude -p --resume`, hook `transcript_path`, and the Agent SDK `getSessionMessages()` [doc sessions; SDK CHANGELOG.md:1128, 982, 1034].

**Location.** `~/.claude/projects/<project>/<session-id>.jsonl`. `<project>` is the cwd with every non-alphanumeric character replaced by `-`. Names over 200 characters are truncated and get a hash appended [doc sessions]. [obs] `/hatimthayyil/code/ai-tools` → `-hatimthayyil-code-ai-tools`, and `/home/hatim/.cache/x` → `-home-hatim--cache-x` (the dot becomes `-`). `CLAUDE_CONFIG_DIR` and `CLAUDE_CODE_PROJECT_DIR_NAME` override the location [doc].

**Retention.** Transcripts are swept after `cleanupPeriodDays`, 30 days by default [doc claude-directory#cleaned-up-automatically]. So `ai chat` must copy what it needs; it cannot rely on the transcript staying around.

**Per-session directory** `<session-id>/`:
- `subagents/agent-<id>.jsonl` plus `.meta.json` (fields `agentType`, `description`, `toolUseId`, `spawnDepth`) [obs].
- `tool-results/`: large outputs spilled to disk [doc] [obs].
- `custom-title.json` [obs].

`file-history/<session>/` (checkpoint snapshots) sits outside the project directory [doc].

### Entry types

Counts over 25 ai-tools logs [obs]: `assistant`, `user`, `attachment`, `system`, `file-history-snapshot`, `file-history-delta`, `last-prompt`, `ai-title`, `custom-title`, `agent-name`, `mode`, `permission-mode`, `atis-latch`, `cost-state`, `queue-operation`.

**Chain entries.** Only `user`, `assistant`, `attachment` and `system` carry `uuid` and `parentUuid`. Common fields on them are `sessionId`, `isSidechain`, `cwd`, `gitBranch`, `version` and `timestamp` [obs]. All the other types are uuid-less metadata lines.

**Legacy summary entries.** There are no `type:"summary"` entries in any log on this machine [obs].

**`system` subtypes** [obs]:
- `compact_boundary`
- `turn_duration`
- `away_summary` (recaps)
- `local_command`
- `informational`
- `stop_hook_summary`

**`attachment` entries.** These carry injected context: `hook_additional_context`, `hook_success`, `file`, `skill_listing`, `total_tokens_reminder`, `deferred_tools_*`, `environment`, `queued_command`, `invoked_skills`, `compact_file_reference`, and others [obs].

### One content block per entry

- One API response is split into one `assistant` entry per content block. The entries share `message.id` and chain parent → child [obs: assistant entries hold exactly one block, `thinking`, `text` or `tool_use`; a `message.id` spans 1 to 4 entries].
- **`tool_result` entries** are separate `user` entries (`message.role:"user"`) with content `[{type:"tool_result", tool_use_id, content, is_error}]`.
  - Extra fields: `sourceToolAssistantUUID` and a structured `toolUseResult`. For Bash this holds `stdout`, `stderr` and `interrupted`. For Agent it holds `agentId`, `status`, `outputFile` and so on.
  - In all 945 cases checked, `parentUuid == sourceToolAssistantUUID` [obs].
- **Parallel tool use makes side branches.** The tool_use entries A1 and A2 chain (A2.parent = A1). Result R1 hangs off A1 as a sibling of A2, and R2 hangs off A2 [obs: 32dcb4ea, entries 31aa7a8b/2dbc1fd9/0e315011/50250cb7]. A strict leaf→root walk therefore drops every parallel result except the last. Results must be attached by `tool_use_id` [inf].
- **Thinking.** Thinking blocks hold `{thinking, signature}`. On claude-opus-5-5, `thinking` was empty (signature only) in about 93% of blocks [obs: 1289 empty vs 101 with text]. Treat thinking as absent [inf].
- **Human prompts.** These are `user` entries whose `message.content` is a string, with `origin:{kind:"human"}` and `promptSource:"typed"`.
  - Meta and injected user entries have `isMeta:true`: the `<local-command-caveat>`, plus `<command-name>` and `<local-command-stdout>` for slash commands [obs].

### Sidechains and subagents

Each subagent's transcript is its own file under `<session>/subagents/`.
- Every entry in it has `isSidechain:true`, the parent's `sessionId`, and an `agentId` [obs: 415/415].
- The parent file holds no sidechain entries [obs: 0].
- The link from parent to subagent is the Agent `tool_use` id (`meta.json.toolUseId`) and `toolUseResult.agentId` [obs].

### Compaction in the file

**One session is one file across compactions** [obs: e046d4df has two `compact_boundary` entries at lines 517 and 1195 under one `sessionId`].

**The boundary entry** is `system/compact_boundary`. Its fields:
- `parentUuid:null`, so it starts a new root.
- `logicalParentUuid`: the last entry before the compaction.
- `content:"Conversation compacted"`.
- `compactMetadata`: `trigger`, `preTokens`, `postTokens`, `cumulativeDroppedTokens`, `durationMs`, `preservedSegment{headUuid, anchorUuid, tailUuid}`, `preservedMessages{...}` [obs, 2.1.220 to 2.1.296].

**What follows the boundary** [obs]:
1. A `user` entry with `isCompactSummary:true` and `isVisibleInTranscriptOnly:true`. Its text starts "This session is being continued from a previous conversation that ran out of context." It has 9 numbered sections: Primary Request, Key Concepts, Files, Errors, Problem Solving, All user messages, Pending Tasks, Current Work, Next Step. It ends with "read the full transcript at: <transcript_path>" and an instruction to resume without acknowledging [obs 737baae4].
2. The `/compact` command echo entries.
3. `file` attachments for the re-read files.
4. Tool, agent and MCP deltas, `environment`, `model`.
5. The SessionStart(compact) `hook_additional_context`, when such a hook exists [obs/exp].

**Preserved segment.** `preservedSegment.anchorUuid` equals the summary entry's uuid. The head..tail entries are pre-boundary entries kept verbatim and logically spliced after the anchor. They are not rewritten [obs: e046d4df]. The loader presumably re-links them [inf].

**Missing parents.** Some `parentUuid`s reference entries that are not in the file. Readers must tolerate dangling parents [obs; also feasibility.md §2].

### Leaf and active path

- `last-prompt.leafUuid` lags behind the file. It equalled the last uuid entry in only 10 of 25 files [obs].
- **Active leaf.** The last appended non-sidechain chain entry [inf; consistent with the rewind observations below].
- **Active context:** walk `parentUuid` from the leaf back to the most recent `compact_boundary`, splice in the `preservedSegment`, and re-attach parallel `tool_result` siblings by `tool_use_id` [inf].
- **Full history:** continue across boundaries through `logicalParentUuid` [obs field; inf usage].
- **Prior art.** VCC skips the tree. It splits the file chronologically at `compact_boundary` and collapses `isCompactSummary` (VCC.py:266-276, 437).

### Rewind, edit, resume, fork, clear

| Action | On disk |
|---|---|
| `/rewind` → restore conversation, or editing an earlier prompt | Same file. The new prompt becomes a sibling of the old one under the same `parentUuid`, and the old branch stays [obs: a87491ed has 4 multi-child prompt groups, e.g. two prompts 13 s apart under one parent; feasibility.md counts 64 such branches in 24/80 files] |
| `/rewind` → Summarize from/up to here | A targeted compaction in the same session. "The original messages stay in the session transcript" [doc checkpointing#guide-a-summary]. Its on-disk shape was not observed |
| `--continue` / `--resume` | "reopens it under the same session ID and appends new messages" [doc how-claude-code-works#resume-or-fork-sessions]. Two terminals on one session interleave into one transcript [doc sessions] |
| `--fork-session` / `/branch` | New session id and new file. History is copied with the same uuids and `sessionId` rewritten; there is no fork-origin field [doc sessions#branch-a-session; obs feasibility.md §2; obs: no `forkedFrom` key anywhere]. SessionStart `source:"fork"` [doc; SDK CHANGELOG:409] |
| `/clear` | New session id and new file. SessionEnd fires with `reason:"clear"` on the old id, then SessionStart with `source:"clear"` on the new id [exp: 737baae4 → 56f7d034, same PPID]. `CLAUDE_CODE_SESSION_ID` "is updated on `/clear`" [doc env-vars] |
| Superseded transcripts | Files named `<session>.orphaned-*.jsonl` and `<session>.jsonl.superseded-*` are set aside [doc claude-directory] |

## 2. Compaction and hooks

### Triggers and control

- `/compact [instructions]` runs a manual compaction. Auto-compact runs at the auto-compact window, which by default is the model's context limit (about 967K on 1M models) [doc model-config].
- Window controls [doc model-config, env-vars]:
  - `autoCompactWindow` setting
  - `/autocompact` and `--autocompact`
  - `CLAUDE_CODE_AUTO_COMPACT_WINDOW` (100k to 1M)
  - `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` (can only lower the threshold)
- `DISABLE_AUTO_COMPACT=1` disables auto compaction and leaves manual `/compact`. It overrides the `autoCompactEnabled` setting [doc env-vars].
- `DISABLE_COMPACT=1` disables all compaction [doc env-vars].
- With auto-compaction off, "sessions stop … with the context-limit error instead of compacting" [doc model-config].
- Every compaction on this machine was `trigger:"manual"` [obs: 42 boundaries].
- "Compact Instructions" in CLAUDE.md, or `/compact <focus>`, steer what the native summary keeps [doc how-claude-code-works#when-context-fills-up].

### PreCompact [doc hooks#precompact]

- **Matchers:** `manual`, `auto`.
- **Input:** the common fields plus `trigger` and `custom_instructions`. `custom_instructions` is the `/compact` argument, and `null` for auto.
- **Blocking:** exit 2, or `{"decision":"block"}`, blocks the compaction.
  - A proactive auto compaction that is blocked is skipped.
  - One blocked during recovery from a context-limit error lets the request fail.
- **Output:** `systemMessage` and `continue` are discarded.
- **No way to supply or replace the summary.**
- [exp]: blocking by exit 2 and by JSON both work, with stderr "Compaction blocked by PreCompact hook".
  - Plain PreCompact stdout did not enter the summary.
  - It surfaced only in the `/compact` `<local-command-stdout>` echo ("Compacted PreCompact [...] completed successfully: PRECOMPACT_STDOUT_MARKER …").
  - That echo is a chain entry after the summary, so the model may see it on manual compactions. This is not a supported channel [inf].

### PostCompact [doc hooks#postcompact]

- **Input:** `trigger` and `compact_summary` (the generated summary text).
- **Control:** no decision control, so it cannot affect the result.
- [exp] It fired after SessionStart(compact): 12:43:13.933 vs 13.964, with `compact_summary` length 3903.

### SessionStart [doc hooks#sessionstart]

- **Sources:** `startup`, `resume`, `clear`, `compact`, `fork`.
- **Output fields:**
  - `additionalContext`: wrapped in a system reminder and inserted where the hook fired.
  - `initialUserMessage`: `-p` mode only.
  - `sessionTitle`: startup, resume and fork only.
  - `watchPaths`, `reloadSkills`.
  - Plain stdout also becomes context.
- **Re-injecting after compaction** is the documented pattern [doc hooks-guide#re-inject-context-after-compaction].
- **Resume inputs:** on resume or fork it also receives `seconds_since_last_response`, `context_tokens`, `prompt_cache_likely_expired` and `estimated_cache_write_usd`.
- **Persisting env vars:** `CLAUDE_ENV_FILE` lets it export env vars into later Bash commands.
- [exp] Injection verified for `startup`, `compact` and `clear`, and for `resume` (a 9,994-character value delivered intact).
  - After `/clear`, the hook injected the previous transcript path, which a SessionEnd hook had recorded keyed by PPID.

### Other events

- **UserPromptSubmit:** input `prompt`. Exit 2 blocks the prompt. It can add `additionalContext` or stdout, set `sessionTitle`, and cannot rewrite the prompt [doc].
- **Stop:** input `stop_hook_active`, `last_assistant_message`, `background_tasks`, `session_crons`.
  - `decision:"block"` with a `reason`, or `additionalContext`, continues the turn, capped at 8 consecutive continuations [doc].
  - [exp] an async Stop hook can spawn a detached worker, including `claude -p`, that outlives the hook.
- **SessionEnd:** reasons `clear`, `resume`, `logout`, `prompt_input_exit`, `other`. No decision control. Default budget 1.5 s, raisable with a per-hook `timeout` up to 60 s or with `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS` [doc].
- **Common input** for all hooks [doc hooks#common-input-fields; exp]:
  - `session_id`, `prompt_id`, `transcript_path`, `cwd`, `scratchpad_dir`, `permission_mode`, `effort`, `hook_event_name`.
  - Subagent hooks add `agent_id` and `agent_type`.
  - `transcript_path` "is written asynchronously and may lag the in-memory conversation".

### What survives compaction [doc context-window#what-survives-compaction]

- **Kept:**
  - the system prompt and output style
  - project-root CLAUDE.md and unscoped rules (re-injected from disk)
  - auto memory
  - a fresh git status snapshot
  - the plan file
  - up to 5 recently read or edited files (those over 5k tokens become a `Referenced file` path)
  - invoked skill bodies (5k tokens each, 25k total)
  - reminders about background tasks
- **Summarized away:** hook-added context from earlier.
- **Re-run:** SessionStart(compact) hooks, whose output is added.
- **Reloaded on demand:** path-scoped rules and nested CLAUDE.md.

### Can we replace the native summary?

**No supported mechanism.** PreCompact can only block. PostCompact is observe-only. The model writes the summary [doc; exp].

Levers we do have:
- Steer it with Compact Instructions or `/compact <instr>`.
- Inject our own view right after it with SessionStart(compact).
- Disable or block auto compaction. That ends in a hard context-limit error, so it cannot replace compaction outright [doc].

### Can the agent or a skill trigger it?

- "A few built-in commands are also available through the Skill tool, including `/init` and `/security-review`. Other built-in commands such as `/compact` are not" [doc skills].
- `/clear` is not listed. Hooks cannot run slash commands [doc hooks: no such field].
- So only the user, or a `-p`/SDK host, can start `/compact` or `/clear` [inf].
- A skill can run `` !`ai chat compact` `` before its body reaches the model [doc skills#inject-dynamic-context]. It cannot then chain `/compact`.

### `/clear` with SessionStart(clear) as "compact with our own summary"

**Viable, verified end to end** [exp]:
1. SessionEnd(clear) sees the old `transcript_path`.
2. SessionStart(clear) on the new session injects arbitrary text.

The new context then holds:
- the system prompt
- CLAUDE.md and memory
- our injected view (≤10k characters per value)

It has no native summary and no re-read files [inf from doc: a fresh session].

Costs:
- The user has to type `/clear` (or a custom `/compact`-like command must tell them to).
- The new session id splits the thread across files. Link the files by recording old → new in SessionEnd/SessionStart. There is no on-disk link otherwise [exp].

## 3. How a CLI run from the Bash tool finds its session

- **`CLAUDE_CODE_SESSION_ID`** is set in Bash, PowerShell, hook and stdio-MCP subprocesses.
  - It matches the hook `session_id` and is updated on `/clear`.
  - On `--continue` or a bare `--resume`, MCP servers "may receive the initial startup ID" [doc env-vars].
  - [obs] It is present in this shell, along with `CLAUDECODE=1`, `CLAUDE_CODE_CHILD_SESSION=1`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_PID` and `CLAUDE_EFFORT`.
- **Subagents.** A Bash call inside a subagent gets the parent's session id. No agent-id variable exists [obs: this research subagent sees `799603ff…`, and its own transcript is `799603ff…/subagents/agent-*.jsonl`].
  - A CLI therefore cannot tell from the environment whether it runs in a subagent [inf].
  - `CLAUDE_PROJECT_DIR` is documented for hooks only [doc hooks].
- **Path resolution:**
  - `${CLAUDE_CONFIG_DIR:-~/.claude}/projects/<cwd with [^A-Za-z0-9]→'-'>/<CLAUDE_CODE_SESSION_ID>.jsonl` [doc sessions].
  - Use the session's start directory, not the current cwd. `/cd` moves the session's storage [doc sessions]. When the encoded name is over 200 characters it carries a hash, so a fallback globs `projects/*/<id>.jsonl` (ids are unique) [inf].
  - A SessionStart hook can write `transcript_path` into `CLAUDE_ENV_FILE`, for example as `AI_CHAT_TRANSCRIPT`, which gives Bash the exact path [doc hooks#persist-environment-variables; inf].
- **Lag.** The transcript is written asynchronously. A CLI reading mid-turn may not yet see the current turn's last entries, including its own `tool_use` [doc common-input-fields; inf].

## 4. Size limits and caching

**Size cap.** "A hook's `additionalContext`, `systemMessage`, and `initialUserMessage` strings, and its plain stdout, are capped at 10,000 characters."
- Each field is measured separately. Over the cap, the value is saved to a file and replaced with its path and a preview of up to 2,000 characters. "Claude Code doesn't ask Claude to read the file". No setting raises the cap [doc hooks].
- When several hooks return `additionalContext`, Claude receives all of them [doc].
- [exp] 9,994 characters was delivered verbatim. About 11.8 KB became `<persisted-output>Output too large (11.8KB)…` with a 2 KB preview.
- A view larger than 10k characters can therefore be split across several hooks or fields [inf from "all values"; untested].

**Caching.**
- Prefix caching applies.
- Injected context is appended as conversation content at the point where it fires. It sits after the system prompt and project context, so it is cached with the conversation from the next request on [doc prompt-caching#how-the-cache-is-organized].
- Compaction invalidates the conversation layer by design. The post-compact turn rebuilds the cache only for the short new history [doc prompt-caching#compacting-the-conversation].
- `/clear` likewise rebuilds the project-context and conversation layers [doc].
- Neither hook-injection path costs a cache miss beyond the one compaction or clear already causes [inf].
- Subagent context re-injection avoids duplicating an existing copy so the cache is kept [doc hooks, SubagentStart].

## Implications for `ai chat`

1. **Ingest from the transcript, read-only and defensively.**
   - Locate it through `CLAUDE_CODE_SESSION_ID` plus the project-dir rule, with the hook `transcript_path` preferred when available.
   - Parse per entry, not per message: merge `assistant` blocks by `message.id`, and attach `tool_result` user entries by `tool_use_id`. That matches the roll-up plan, where a tool run folds into the surrounding assistant turn.
   - Drop `isMeta`, attachments, metadata lines, and empty thinking.
   - Treat `isCompactSummary` as a Claude-authored node, not a user turn.
   - Follow the active branch: last appended leaf, `parentUuid`, `logicalParentUuid` across boundaries.
   - Keep abandoned rewind branches as siblings.
   - Ingest subagent files as child threads keyed by `toolUseId`.
   - Pin to a tested `version` range and fail loudly on unknown shapes; the format is explicitly unstable.
   - Copy everything into the repo-local git store promptly, because of the 30-day sweep.
2. **Recommended integration: native compaction plus our view injected after it.**
   - **PreCompact** (no block): run `ai chat ingest --transcript "$transcript_path"` synchronously, so the store is current at compaction.
   - **CLAUDE.md "Compact Instructions"**: ask for a minimal native summary (current task, next step) that defers to `ai chat`, to save tokens.
   - **SessionStart(compact)**: print `ai chat view` (the rolled-up tree top, ≤10k characters) as `additionalContext`, including a hint that `ai chat zoom <id>` expands nodes.
   - Also hook SessionStart(resume|clear|fork) and SessionEnd for continuity across session ids and files.
   - Optionally a Stop hook (async) for incremental ingest.
   - This is fully documented, works for auto and manual compaction, and costs no extra cache miss.
3. **Not recommended as the default:** `/clear` + SessionStart(clear). It gives a pure `ai chat` context with no native summary, and is verified to work. But it needs the user to type `/clear`, loses the re-read files and skill bodies, and starts a new session file each time. Offer it as an opt-in "hard reset" mode, documented as `/clear` after `ai chat compact`.
4. **Ruled out:**
   - Replacing the native summary text (no hook can).
   - Agent-triggered `/compact` or `/clear` (not reachable through the Skill tool).
   - Disabling auto-compact as a primary mechanism (it ends in a context-limit error).
   - Writing into Claude Code's JSONL (unsupported).
5. **Budget the injected view to 10,000 characters per field.** Anything larger is truncated to a 2 KB preview that Claude is not told to read.
