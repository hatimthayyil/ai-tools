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

## Open

- The command's form. Proposed: `/fold <direction>`, with bare
  `/fold` unchanged.
- Whether bare `/fold` writes a final message by default.
