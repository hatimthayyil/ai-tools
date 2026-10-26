# AGENTS.md - ai-tools

## App

`ai`: all-in-one AI tool.

- `ai memory`: permanent memory for AI agents

## Working

- Delegate implementation work to dedicated subagents. Follow logical sequencing when there are dependencies between tasks. Multiple agents can be spawned or work that can be done parallely.
- AI are provided with a workspace. This is the primary home for any AI assistant. `ai`, `ai/research`, `ai/comments`, etc.
- Humans are provided with a workspace. This is their primary home. Do NOT edit anything in this directory. `my`, `my/notes`, etc.
- The main tree is for AI-assisted files. These are worked on by BOTH AI and humans. And should have minimal changes.
- Glossary is given in `my/glossary.md`. Maintain `ai/glossary.md` yourself.
- Backlog is given in `ai/backlog.md`.

## Development

- This repo is in active development. Expect high churn: commands, file formats and code change often, and that is fine.
- Never cut corners. Build each change completely and correctly: handle every case and failure, and test it.

## Writing
- Prefer terse prose
- Write research notes in `ai/research`
- When naming files that would benefit from chronological sequencing or keyword identifications, follow the Denote naming pattern. eg: `YYYYMMDDThhmmss--<slug>__<keywords>`. Keywords are optional and separate by `_`. Only add keywords from the "Keywords" section in `ai/glossary.md`.
