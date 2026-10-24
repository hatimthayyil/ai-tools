# store.rs

Spec: `ai/research/20261013T222521--notes-design.md` (Store). Layer 1.

- Non-bare repo at `~/.ai/notes` (`AI_NOTES_DIR`): the files are the source of truth, readable and editable by hand. Reads (`load`) go to the work tree, not to git objects; hand edits are swept into the next commit.
- `mutate`: takes `.git/ai-notes.lock` (blocking `flock`), loads the tree, lets the command edit files, then commits. Serialised writers instead of memory's lock-free CAS: a commit here rebuilds the tree from the work tree, so two writers at once would race on the files themselves, not only on the ref.
- The commit is `repo::commit` (repo.rs.md): the whole work tree, no commit when unchanged, the index written with stats so `git status` is clean. Fallback committer: `ai notes <ai@localhost>`.
- `create`: on the first write, through `repo::create`. A dir with other files and no `.git` is refused (files are listed before `.git` is checked: a winner renames `.git` in before writing any).
- `load`: directories named by slugs are topics (others ignored, so `.git` and stray dirs are skipped); files `<id>--<slug>.md` are notes, `_summary.md` the summary (none at the root). Sorted by name, so ids order notes by creation.
- `prune`: after a move, the source topic and each one above it holding nothing but `_summary.md` are removed. git keeps no empty dirs, and a summary of nothing would linger otherwise.
- `write` goes through a `.tmp` file and a rename: reads take no lock, so they must never see half a file.
- Locks and the summarise log live in `.git`, so they are never committed.
