# Mail delivery in Claude Code

Author: Claude Code (Claude Opus 5.5)

How Claude Code records a message the `ai` plugin delivers, and what the delivery layer can build on. It serves `20261030T142550--agent-team.md` §4 (delivery through `$.prompt.submit` plus the Stop hook). Nothing here is built.

Sources: Claude Code 2.1.314; its plugin engine types, `plugin-authoring/types/claude-code.d.ts` ("types"); its docs in `~/.claude-code-docs/docs`. **Checked here** marks what two headless sessions (`claude -p`, Haiku) on this machine showed: a throwaway plugin `mailprobe` loaded with `--plugin-dir`, and a Stop command hook passed with `--settings`. The probes are in the scratchpad, not the repo. Everything else was read.

## Findings

### 1. A plugin's prompt in the transcript

`$.prompt.submit({ text })` is "a turn of its own, once the session is idle" (types:3027). Its user entry carries an origin the engine sets and no hook may change (types:9674):

```json
{"type":"user","origin":{"kind":"plugin","name":"mailprobe"},"promptSource":"system",
 "turnOrigin":"system","promptRewritten":true,
 "message":{"content":"The mailprobe plugin sent a message:\n<ai-mail id=\"message_idle\" …>\n…\n</ai-mail>\n\nThis is how Claude Code surfaces a prompt a plugin submits between turns — it starts this turn in the user's place. Address the message above."}}
```

- The content is framed: the text is wrapped in a prefix naming the plugin and a fixed suffix (types:9441). **Checked here.**
- With `asUser: true`, the content is the text alone, and the origin adds `"asUser":true` (types:9548). **Checked here.**
- `isMeta` is absent. **Checked here.**

Every source looks different (**checked here**; typed prompts from this session's transcript):

| Source | `origin` | `promptSource` |
| :-- | :-- | :-- |
| typed at the terminal | `{"kind":"human"}` | `typed` |
| `claude -p`, SDK | none | `sdk` |
| plugin `$.prompt.submit` | `{"kind":"plugin","name":…}` | `system` |

So the reader can tell delivered mail from the user's words, and from other plugins' prompts, by `origin.kind == "plugin"` and `origin.name == "ai"`.

**Today the reader drops such a turn.** `user()` (src/agent/claude.rs:331) finds no `work()` match, since `work()` knows only `peer`, `coordinator` and `task-notification` (:378-395), and `prompt()` accepts only a `human` origin or an `sdk` source (:397-402). Nothing becomes an item.

### 2. Submitting while idle, and during a turn

- **Idle, triggered inside the plugin: works.** A `$.clock.after(40000, …)` set at `session.start` fired while the session waited for input; its submit started a turn at once, and the model answered it. **Checked here** in a stream-json session; an interactive terminal session was not tried.
- **During a turn: queued, never merged.** A submit made at the first turn's `turn.start` raised `prompt.submit` only after that turn ended (6.5 s later), then ran as a turn of its own (`turnPosition.turnIndex` 2), ahead of the next SDK input. **Checked here.** The types say the same: a plugin's prompt carries no `turnId` and "runs once it is idle" (types:9658).
- `$.prompt.submit` resolves when the prompt enters or is queued, not when its turn ends (types, `PromptSubmitResult`).
- **No file watch in the API.** `$.fs` offers read, write, list, exists, stat and ancestors (types:3336-3460). Ways to notice new mail:
  - poll with `$.clock.every` (types:3577) and `$.fs.stat` (:3434): timers fire while idle (**checked here**, for `$.clock.after`);
  - `$.process.spawn` a long-running watcher and read its output (types:3666), as `register.ts` already reads `ai agent compact-session`;
  - return `watchPaths` from the classic SessionStart hook and handle `classic.FileChanged` (hooks.md, "FileChanged"; types, `ClassicResultFields`). Not tried.

### 3. Events for a turn ending or the session going idle

- `turn.complete`: fires when a model turn ends, with a `reason` (types:4623). **Checked here** (`reason: "answer"`).
- `classic.Stop`: the settings Stop hook as an event.
- `session.measure`: after each main-thread turn (types:4567). Not tried.
- Classic `Notification` with `idle_prompt`: about 60 s after a turn, only when the user seems away (hooks.md:2274, :2298).
- `TeammateIdle`: agent teams only.

There is no "session is idle" event and no op that says whether a turn runs. None is needed for delivery, since `$.prompt.submit` waits for idle itself.

**The tier matters for classic hooks.** On this machine the built-in `cc-plugin-sec-default` sits outermost, because managed settings exist, and skips every classic hook of a user-tier plugin. The debug log read: `mailprobe: classic.Stop bypassed by cc-plugin-sec-default (tier user); beneath runs`. **Checked here.** `ai` is installed through managed `appendPlugins` (`/etc/claude-code/managed-settings.json`), at tier `append`, so it gets classic events. `$.prompt.submit`, `turn.start` and `turn.complete` worked at tier `user`.

### 4. A Stop hook's block reason

`decision: "block"` with a `reason` (a module's `{ block }`, types:1312-1317) writes a user entry with `isMeta: true`, no origin, and the content `Stop hook feedback:\n` followed by the reason:

```json
{"type":"user","isMeta":true,"message":{"content":"Stop hook feedback:\n<ai-mail id=\"message_block_small\" …>\n…\n</ai-mail>"}}
{"type":"attachment","attachment":{"type":"hook_blocking_error","hookEvent":"Stop","blockingError":{"blockingError":"<ai-mail …","command":"node …/stop-hook.mjs …"}}}
```

- **Verbatim, at least to 131,072 characters.** The content after the prefix was byte-equal to the reason, and the model received it whole (226k input tokens). **Checked here.** No cap is documented for `reason`.
- Each block continues the same turn with `stop_hook_active: true`; Claude Code ends the turn after 8 consecutive blocks (hooks.md:2554).
- The `hook_blocking_error` attachment makes the terminal show a hook error.
- The reader drops it: `user()` returns nothing for `isMeta` entries (src/agent/claude.rs:338-340).
- Tested with a settings command hook. A plugin module's `{ block }` folds into the same classic result (types:1312), but what `blockingError.command` holds for a module was not seen.

**Stop's `additionalContext`** is the other route. It becomes a `hook_additional_context` attachment (`hookEvent: "Stop"`), its `content` an array of strings, and the turn continues. It is capped at 10,000 characters: past that the transcript holds a `<persisted-output>` path and a 2,000-character preview (hooks.md:941-944). **Checked here** (13.9 KB became 2,311 characters). A long message would lose its body from the record, so this route doesn't fit mail.

The docs label `additionalContext` "Stop hook feedback" (hooks.md:2633). The transcript gives that label to the block's entry instead.

### 5. The envelope

The envelope `<ai-mail id=… from=… approved=…>` held quotes, `&`, `&lt;/ai-mail&gt;`, a tab and Arabic text. It arrived unchanged in every path:
- the framed submit, between the frame's prefix and suffix;
- the `asUser` submit, alone;
- the Stop block, after `Stop hook feedback:\n`;
- Stop `additionalContext`, while it was under 10,000 characters.

**Checked here.** A reader can take the envelope out of any of these by its tags.

## What the delivery layer should build on

1. **`$.prompt.submit` alone covers both states.** Idle, the message starts a turn at once. Busy, it queues and runs as its own turn when the running turn ends: the moment a Stop block would deliver it. The submit carries an engine-set origin naming `ai`, its own turn and no hook error. A Stop block adds weaker provenance (no origin), an error notice, a cap of 8, and a turn the message shares with the agent's previous work. The agreed design pairs `$.prompt.submit` with the Stop hook (team note §4); the evidence here says the Stop hook is not needed.
2. **Keep the frame.** The framed form tells the model the text came from the `ai` plugin, not the user. The reader strips it by taking the envelope out.
3. **The reader treats an envelope as a pointer, not as the record.** A turn becomes `Kind::Work { from, text }` only when:
   - its origin is `{"kind":"plugin","name":"ai"}`;
   - it holds an envelope;
   - the envelope's id names a message approved for this agent in the store.

   The text then comes from the store. That keeps a typed or quoted envelope, or one from another plugin, from passing as mail.
4. **Notice new mail** by polling: `$.clock.every` with a cheap `$.fs.stat` of the inbox. Also check at `session.start` (a resumed agent) and at `turn.complete`. `classic.FileChanged` with `watchPaths` would replace the poll if it proves to work for a plugin.
5. **Avoid double delivery.** `$.prompt.submit` resolves before the turn runs, and the transcript shows the item only once the turn is written. The plugin keeps the ids it submitted in `$.state` until the reader sees them.

### Under the identity note's sandbox

`20261030T200814--agent-identity-and-enforcement.md` gives each agent its own `CLAUDE_CONFIG_DIR` and a read-only inbox.
- **Same format, new path.** Transcripts and persisted outputs move under `$CLAUDE_CONFIG_DIR/projects/<dir>/` (src/agent/claude.rs:26-31). A reader outside the sandbox finds them through the transcript path the binding records.
- **The recipient writes nothing back.** Delivered state comes from the thread, as point 3 needs; the plugin only reads the inbox (`$.fs.stat`, `$.fs.read`, `ai`).
- **The sandbox must keep `/etc/claude-code` and the plugin's files visible.** `ai` loads only through managed `appendPlugins`, and only that seat gives it classic hooks. Not tried: whether a fresh `CLAUDE_CONFIG_DIR` finds the `ai` marketplace and plugin.

## Not verified

- Submitting while idle in an interactive terminal session: only stream-json was tried. The engine's queue is the same, and the types promise it.
- A plugin module's `{ block }` at tier `append`: the probe used a settings command hook.
- `classic.FileChanged` and `watchPaths` for a plugin.
- The order of a plugin's queued prompt and a prompt the user typed during the same turn.
- Timers over long idle periods (the probe idled 60 s).
