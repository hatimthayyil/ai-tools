# cli.rs

- `ai notes` with no subcommand is `view`.
- `Runtime`: backend, `summarise_on_change` (unless `AI_NOTES_SUMMARISE=0`), stdin. Smaller than memory's: no compactor options or lock seam.
- Summaries run as in memory: a change (`add`, `mv`) commits, then, if a summary is due, spawns a detached `ai notes build` (memory's `spawn_summariser`, logging to `.git/summarise.log`) and returns. `link` never dirties a summary, so it spawns nothing.
- `build`: one summariser per store (`.git/ai-notes-summarise.lock`, try-lock: exit if held). Rounds: build every due summary at once (up to memory's `JOBS` threads), write each under the write lock only if the topic's input still equals the one summarised (else it is redone next round), one commit `summary <topic>` each. After releasing the lock it re-checks, as memory's summariser does, so a change whose summariser found the lock taken is still built. A failed call fails the summariser; the next change retries.
- `add <topic> [text]`: body from the argument, else (or `-`) stdin. `--title` and `--confidence` required; `--source`, `--checked`, `--decay` one line each; `--link` ids must exist. One commit `add <topic>`.
- `mv <from> <to>`: an id moves a note into topic `to` (under its title's file name); a topic is renamed to `to`, which must not exist (no merge semantics) nor lie inside it. Emptied source topics are pruned. One commit.
- `link <a> <b>`: appends `b` to `a`'s `links`, directional (backlinks are a lint, layer 2). Idempotent: an existing link makes no commit.
- `view`: topic lines `topic (valid notes below): scope`, indented by depth, deepest whole level that fits `VIEW_MAX` (8000 bytes, ~2k tokens); a level that alone overflows is cut, with a count. An unsummarised topic shows memory's `PLACEHOLDER`. A topic whose summary is a copy of its one subtopic's (`only_child`, summary.rs.md) is not printed: the chain shows as one line named by its deepest topic (`linguistics.syntax (3): …`), as file browsers compact single-child folders. Only exact copies collapse, so a stale or unsummarised parent still shows.
- `zoom <topic>`: the topic's line and gist, each subtopic's line and gist, then its notes as `id [confidence] title`.
- `grep`: memory's `Pattern` (smart case, `-F`, `-i`, `-s`) over note titles and bodies and summary scope+gist, every hit with its breadcrumb (`a › b: <excerpt>`, notes `a › b › <id>: <title>`). Tree order. Only the deepest hit of a chain prints: a summary hit is dropped when a note of its topic, or anything below it, matches too, since summaries restate what is under them.
- `show <id>`: the file's path on disk, the note (frontmatter, `# title`, body), then each link resolved to `breadcrumb › id: title`.
- No usage log (memory's `usage.rs`) yet.
