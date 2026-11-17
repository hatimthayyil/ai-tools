# Fold with a direction

## Problem

Keeping one's place across a fold takes four steps:

1. Ask the agent to gather what we'll need into a final message.
2. Wait for that message.
3. Run `/fold`.
4. Tell the new session what to do. It then spends its first turns
   zooming and grepping to rebuild the details.

## Want

One command that takes where the user is going next. The session
being folded still holds every detail, so it writes the final
message, aimed at that direction. The fold carries the message into
the new session word for word. The new session then starts on the
direction straight away.

## Uses

One command, with different directions:

- Continue: `carry on with the fix pass`.
- Refocus: `pick up the lawcheck setup`. The final message gathers
  what bears on lawcheck, zooming into older sessions where needed,
  not the whole conversation.
- Conclude: `implement the agreed design`. The final message records
  the decided design, with its choices, reasons and rejected options,
  so implementation keeps them.

## Gather before the fold

The old session holds the details itself. The new session has only
the rolled-up view, which loses detail, so it has to zoom. The final
message survives the fold intact, because the view shows the newest
turns word for word.

## Design

`/fold [direction]`. Bare `/fold` writes a handoff too; a direction
aims it.

The handoff is state, not history: where things stand, decisions with
their reasons, the plan, facts the next session needs, and what is open
with the user, each cited by turn, item or note id. The thread keeps
the history, and the view shows the newest turns word for word, so the
handoff leaves out what they say. Its instructions are
`plugins/claude/prompts/fold.txt`.

Steps:

1. `/fold` builds the thread's view. A failure stops it before any turn
   is spent; Esc cancels it.
2. The command leaves the model the instructions as its context, then
   sends "Fold this session." as the user's words (`asUser`), with
   "Next:" and the direction after it when there is one. The thread
   counts that prompt as the user's message, so the handoff is its
   answer.
3. When `turn.complete` hears that turn end with an answer, the hook
   builds the view again, now ending in the handoff, and folds as
   before. A stopped, refused or failed turn folds nothing.
4. The new session starts on the direction, sent as the user's words.
   One that holds no view or is attached to no agent, or a direction
   refused, leaves the direction in the prompt box.
5. A fold that stops short of starting a session on the direction
   hands it back: into the prompt box, after a blank line when the box
   holds a draft, else written out whole in the message saying why.
   This covers every stop: a build that fails or is cancelled, a fold
   already under way, unreadable instructions, a refused request, a
   handoff turn with no handoff, and a failed view, `/clear` or
   `/compact`.

## Constraints

Found on Claude Code 2.1.326:

- A plugin's own `$.command.run` and `$.prompt.submit` never reach that
  plugin's hooks for the same event, so the fold cannot rerun `/fold`,
  and the instructions cannot ride on the request through a
  `prompt.submit` hook. They go as the command's context, an `isMeta`
  row the thread leaves out.
- The turn waits on its `turn.complete` hooks, and `$` calls cost a
  hook no budget, so the second build runs there. Claude Code shows the
  turn as done meanwhile, and Esc does not reach the hook, so the slow
  build stays in the command and the second only adds the handoff.
- A prompt typed while the second build runs waits, then runs in the
  old session before the `/clear`.
- Claude Code keeps no record of a plugin command's arguments but the
  transcript's: `~/.claude/history.jsonl` leaves out commands of type
  `local`, which every plugin command is. A direction the plugin does
  not hand back is gone.
