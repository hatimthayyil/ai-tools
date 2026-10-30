# AGENTS.md - ai-tools

## App

`ai`: all-in-one AI tool.

- `ai memory`: permanent memory for AI agents

## Working

- Delegate implementation work to dedicated subagents. Follow logical sequencing when there are dependencies between tasks. Multiple agents can be spawned or work that can be done parallely.
- AI are provided with a workspace. This is the primary home for any AI assistant. `ai`, `ai/research`, etc.
- Humans are provided with a workspace. This is their primary home. Do NOT edit anything in this directory. `my`, `my/notes`, etc.
- The main tree is for AI-assisted files. These are worked on by BOTH AI and humans. And should have minimal changes.
- Glossary is given in `my/glossary.md`. Maintain `ai/glossary.md` yourself.
- Backlog is given in `ai/backlog.md`.
- Use one GitButler branch per feature. Forks commit onto that branch. Do not create a branch for a task, a fork or a single file. Create a second branch only when the user asks.

## Development

- Hatim is the first and main user, and Hatim's vision sets the direction: it decides what gets built and in what order.
- Before v1, change is cheap. Commands, file formats and behaviour change often, and that is fine: when the vision needs something different, change it. No compatibility layers, migrations or deprecation periods, and no hesitating because something already works.
- Good design still counts: usability, sensible defaults, clear behaviour, and choices that serve anyone else who uses `ai` or works in a repo that does.
- Breakage is accepted, not ignored: mark a breaking commit with `!` and a `BREAKING CHANGE:` footer, and put upgrade steps in a `docs(upgrade):` commit, so the changelog shows both. Don't let breakage stop the change. After v1, breakage becomes a real cost.
- Never cut corners. Build each change completely and correctly: handle every case and failure, and test it.

## Writing
- Prefer terse prose
- Write research notes in `ai/research`
- When naming files that would benefit from chronological sequencing or keyword identifications, follow the Denote naming pattern. eg: `YYYYMMDDThhmmss--<slug>__<keywords>`. Keywords are optional and separate by `_`. Only add keywords from the "Keywords" section in `ai/glossary.md`.
