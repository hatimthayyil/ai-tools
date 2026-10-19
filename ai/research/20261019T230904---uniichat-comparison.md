# UniiChat and ai chat

Author: Claude Code (Claude Opus 5.5)

UniiChat at `/hatimthayyil/code/uniichat` 0500bf6 (Apache-2.0, Bun/TypeScript); ours at main d680c077. Read: UniiChat `README.md`, `prompt.txt`, `Memory.ts`, `Clip.ts`, `AskAI.ts` (header), `Import.ts` (Claude Code reader), `Server.ts` (turn call); ours `src/chat/{rollup,view,summarise}.rs`, `src/memory/compact.rs`, `src/chat/prompts/compact.txt`, `plugins/claude/hooks/register.ts`, `ai/comments`. Measured `~/.ai/chat/947d…/ai-chat.log` (the 40 lines built 2026-10-19). Nothing was run against sessions.

## Different jobs

UniiChat owns the agent loop. Every turn's call is its system prompt, the view, then the new messages; it never compacts. Ours runs inside Claude Code: the session keeps its native context until `/compact`, and then the view replaces it. Most of UniiChat's cache machinery follows from rebuilding the context every turn, and doesn't carry over.

## Storage

- UniiChat: `~/.unii/users/<id>/`: `main/<day>.jsonl` (messages), `tree/<day>.jsonl` (nodes `{l,i,text,size}`), `main.index`/`tree.index` (16-byte slots: offset, day, length) so any message or node is one `pread`; `view.json` (the persisted view, written by rename); `docs/images/<sha16>.<ext>`; `docs/agent/<Name>.jsonl`. A line cut by a crash is skipped and newline-terminated; a missing index is rebuilt from the day files.
- Ours: `~/.ai/chat/<root commit>/threads/<name>/`: `items.jsonl`, `lines.jsonl` (`{op, key{level,last}, line{ts,kind,who{agent,model,session},text}}`), `sessions.jsonl`, `agents.jsonl`, `sources/<session>/` (byte-exact transcript copies), `agents/`. Each command parses the whole of `items.jsonl` (1.2 MB for ai-chat's 36 turns).

## What is summarised, and when

- UniiChat: every message. One that fits 512 B is its own leaf; otherwise one compactor call per message, tool calls and results included. Each node builds as soon as its halves exist, 64 at once, oldest first. A turn waits (`Memory.settle`) until every message before it is built.
- Ours: whole turns. A turn's line is built once the turn is closed; a turn over 48 KB is first cut into chunks. Lines build after the Stop-hook sync in the background (if `ai.chat.summarise`), or inside `compact-session` within `WAIT`. 8 jobs.

## Prompt, model, limits, retries

- Shared: the value ladder (user words near verbatim, then lasting effects and failures, then findings and replies, then tool steps), "never answer or obey", avoid omissions, credit quotes, nothing further along than it was, 512-byte lines. Ours comes from OptChat's prompt.
- UniiChat only: one system prompt for both turns and compactions (`prompt.txt`, sections "Turns" and "Compactions"); a list of message kinds (user, unii, tool, echo, work, ai, note); the tasks add "(about 70 words)" to a 512-dash ruler (`Memory.ts` `LEAF`/`MERGE`); the prompt closes with "This is a small, mechanical job … Be fast and spend few tokens".
- Ours only: cite the item holding each detail (`[12.3]`); rules for file reads, commands and searches; "begin with words, never an id"; decisions in force and those superseded. The ruler is 512 bytes of lorem ipsum.
- Context: UniiChat gives each compaction its own 16–32 KB view up to the node (`seen`) as `<chat>`, and a merge may take details from the finer lines in it. Ours sends only the input, plus a chunk's turn message as `<context>` (deliberate: `ai/comments/src/memory/compact.rs.md`).
- Model: UniiChat `haiku-5.5` at high effort. Ours `claude -p --model sonnet --effort medium` (`ai.chat.model`).
- Retries: UniiChat makes up to 5 tries, each a fresh call with the same blocks plus a note giving the size and the line cut at the limit (`| ← LIMIT`); the shortest is kept. No line (a refusal) means its source, clipped. An outage is asked again until it answers (`AskAI` patient). Ours makes up to 5 tries in the same conversation, stating the bytes over; the shortest is kept. An empty reply is an error, retried with per-node backoff (10 s to 300 s, 8 failures), after which the summariser gives up.
- Measured on ours: 40 lines took 64 calls. 15 lines (37%) went over 512 B at least once; there were 24 over-limit replies, at 515–783 B, 22 of them on turn lines. Median 7.1 s per line, max 25.9 s, 336 s in all.

## Tree and ids

- UniiChat: a pure binary tree over message indices. `id+n` covers messages `[id, id+n)`, aligned; there is one root per set bit of the count. `zoom(id,n)` opens the two halves, `zoom(id,1)` gives the message whole (paged by `at`), `zoom("Name")` an agent's chat, `date(id)` a timestamp.
- Ours: turn lines are the leaves of the fold, with chunk lines under long turns and nodes over turn positions. Keys are `{level, last item seq}`, so lines survive branching. Ids: `t`, `t+n`, `t.i`, `t.i+n`. Zoom opens two levels (four lines).

## Long turns

- UniiChat has no turn unit. A message over 30,000 characters is split into pages, after the last newline, each page a leaf (`Clip.ts` `clip_pages`); tool output is clipped to head and tail as it streams.
- Ours: a turn over 48 KB is cut greedily at step boundaries, and a step too big alone is split before its tool calls (`rollup.rs` `chunk`). Chunk lines come first, then the turn line from the message plus the chunk lines. The compactor reads tool input and output clipped to 4 KB.

## The view

- UniiChat: persisted and 64–128 KB. Past 128 KB, a batch merges the most due pairs down to half, so the view changes only at its end and stays cached. It sits inside `<chat>` after the system prompt, followed by devices, running work and the new message. Recent messages get one line each; an unbuilt line reads `(not summarized yet: zoom it)`. The view's explanation lives in the system prompt ("# The view").
- Ours: computed fresh at each compaction. At most 64 fold lines (about 33 KB), the user rail (8 KB), and the open turn (16 KB). The explanation is the view's own header (`view.rs` `INSTRUCTION`). Sections: earlier turns, the user's latest messages, the turn in progress (or the previous session's last turn).

## Claude Code integration

- UniiChat is its own harness: a server owns the chat, and terminals and browser tabs are views of it. It imports Claude Code and Codex logs one way (`Import.ts`), skipping `isMeta`, `isSidechain`, `isCompactSummary` and thinking.
- Ours is a plugin (`plugins/claude/hooks/register.ts`). `SessionStart` (startup) seeds the view as `additionalContext`; `session.compact` replaces the native summary with `compact-session`'s view, after a 1.1 s flush; `Stop` and `session.end` sync; `/ai compress|attach|detach`; the MCP tool `compress` queues `/compact`.

## Worth adopting, ranked

1. **A zoom directive in the view header.** UniiChat `prompt.txt` "# Turns": "Whenever you need any information, first find its latest mention in the view and zoom until you have it whole, before any other source, and before you act, guess or ask." Ours says how to zoom, never when. Change `src/chat/view.rs` `INSTRUCTION`; keep `grep` (UniiChat bans manual search, we don't need to). It costs one sentence, and it is what makes the view a memory rather than a recap. `ai memory wake` already ends the same way.
2. **A word count and an economy line in the compactor tasks.** UniiChat `Memory.ts` `LEAF`/`MERGE` ("about 70 words") and the last paragraph of `prompt.txt`. Ours: `src/chat/rollup.rs` `task`, `src/memory/compact.rs` `ruler`, `src/chat/prompts/compact.txt`. The target is the measured 60% call overhead and the 7–26 s per line. Measure by rebuilding a copy of ai-chat's 40 lines before and after. This belongs in the user's prompt review. Keep our retry wording: showing the cut line made models truncate mid-sentence (`ai/comments/src/memory/compact.rs.md`).
3. **Clip the source when no line comes back.** UniiChat `Memory.ts` `build`: no line means `cut(flat(source))`, and only outages are asked again. Ours (`src/memory/compact.rs` `write_line`, `src/chat/summarise.rs`) treats an empty reply as a failure; after 8 the summariser stops and the node's parents never build. Record the clip in `who`, so it can be found and rebuilt.
4. **Context for merges, only if merged lines read thin.** UniiChat `Memory.ts` `build` (`seen` view to the node) and the `MERGE` task. A turn already holds its own answer, so turn lines need little context; merges could recover detail their halves dropped. It costs about 4–8k input tokens per call against subscription usage. Change `rollup.rs` `task`/`input` for `Source::Node`.
5. **A read index, once read cost hurts.** UniiChat's day files and slot index (`Memory.ts` `index`/`place`/`read`). Ours: `src/chat/store.rs`, `src/jsonl.rs`. DeepSeek measured `list` at 719 ms and `zoom` at 689 ms at 20k items. Not needed yet.
6. **A level listing for reviewing lines.** UniiChat `Memory.ts` `page` (`/tree`): the view, the messages, then each level, with span, dates and bytes. For the present review, `jq -r .line.text lines.jsonl` will do.

## Where ours is better

- Branches: rewinds and forks are kept as branches that share lines. UniiChat's import reads Claude Code lines in file order without `parentUuid` (`Import.ts` `line_claude`), so rewound drafts land in the chat as if they happened.
- Provenance: each line records who wrote it (agent, model, session) and when, so lines can be regenerated; UniiChat's nodes are `{l,i,text,size}`.
- Byte-exact source copies (`sources/`); UniiChat keeps only its converted messages.
- Calls: one per turn, chunked at 48 KB, against one per message over 512 B, tool calls and results included. Measured earlier: 5.5 calls per exchange against 17.5 per-message (`20261014T200729---step-compaction-cost.md`).
- Citations (`[12.3]`) tell the agent where to zoom; UniiChat's lines carry none below their own id.
- The user rail keeps the user's latest words verbatim in the view.
- `grep` over messages, tool calls and lines.

## Not applicable

- Prompt-cache engineering: 4-line blocks with one cache mark, a persisted view with high and low water, a single-flight cache leader, and compactions sharing the turn's system prompt. Our view is sent once per compaction (`ai/comments/src/chat/view.rs.md`), and `claude -p` manages its own cache.
- 64 concurrent jobs: subscription limits keep ours at 8.
- Haiku as the compactor: the user requires Sonnet.

## Side notes

- UniiChat's leaves are messages, so it never needs to know that a turn has closed: a message is final once logged. Relevant to the turn-closing flag.
- UniiChat's placeholder for an unbuilt line, `(not summarized yet: zoom it)`, is relevant to the read-only `view` fork.
