# files/cli.rs

`ai files build` and `ai files view`: the walk, the store and the summariser joined.

- The project is `Project::find`, the same lookup chat uses, so a change to how projects are found reaches both. The store is `~/.ai/files/<project id>/`, its calls in `calls/` under it.
- `build` runs only when asked: no hook or background run starts it, since a build costs model calls.
- Rounds, as `notes build`: each round summarises what is ready (files, and directories whose children all have summaries), in batches of `JOBS` calls at once. A node whose hash is stored is skipped, and so is everything under a stored directory, its hash covering them.
- Each hash is summarised once per round, so identical files or directories cost one call.
- A failed call stops the build once its batch ends; the batch's other summaries are saved first, so the next build goes on from them. The error names the path and the cause.
- A file is read again for its call and its hash checked against the walk's, so a summary is never stored under the hash of other content.
- `build.lock` keeps one build per project; the store's own lock still guards each write.
- `view` walks the tree as it is now and prints the summary stored under the node's hash. A path with no summary for its current content fails, rather than showing a stale one.
