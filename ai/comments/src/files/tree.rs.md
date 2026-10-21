# tree.rs

The tree `ai files` summarises: text files under a root and the directories holding them. Prior art: `ai/research/20261021T204120--file-tree-summary-prior-art.md`.

- Ignores: the `ignore` crate with only git's sources on (`.gitignore` up from the root, `.git/info/exclude`, the global excludes file), `require_git(false)` so a project that isn't a repository still honours its `.gitignore`. Hidden files stay in, as git keeps them. `.ignore` files (ripgrep's) are not read.
- Extra ignores (for `ai.toml`): gitignore lines relative to the root, checked as the walk descends, so an ignored directory is never entered. They add ignores only: a `!` line cannot bring back what `.gitignore` drops.
- Text: valid UTF-8 with no NUL byte (git's binary test is a NUL). Names that aren't UTF-8 are skipped too, since a summary cites files by path. Symlinks are not followed or listed.
- Hashes are git's: a file's is its blob id (`git::hash`, shared with notes), a directory's the tree id of its listed children, all as plain files. So a directory's hash changes exactly when something under it does, and with nothing ignored or skipped (and no executable bits) it equals `git write-tree`, which a test checks.
- A directory with no text file under it is left out, so every directory has something to summarise.
- Children sort by name, bytewise; the tree id sorts its own entries in git's order.
- Paths use `/`, relative to the root; the root's is `""`.
