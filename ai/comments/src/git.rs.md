# git.rs

Git plumbing of `ai memory` (`memory/store.rs`): a store is a tree of blobs under one ref. Design: `ai/research/20261003T091838--memory-git-ref.md`, versioning `20261013T224252--record-versioning.md`.

- `Ref`: one ref in one repository. `what` names the store in messages ("memory"); `format` is the format this ai writes. The caller sets the committer fallback on the repository first (`committer_or_set_fallback`).
- `Changes`: blobs to write by path.
- `mutate`: one commit per mutation, compare-and-swap on the ref, redone from the new head when another writer wins. Lock-free: concurrent writers never block each other. `at` builds the store's own snapshot type from a commit and runs its format check on every retry; it returns anything that is `AsRef<Snap>`, so memory wraps `Snap` with its own readers.
- No changes: no commit, and the base snapshot comes back.
- The first commit writes the `format` blob: a store always names its format. `parse_format`: absent is 1 (memory before format blobs); 0 or a non-number is corrupt. `readable` refuses a newer format before any write.
- `cas`: a failed ref edit is a lost race when the head moved, an error otherwise.
- `Snap`: one commit's tree; reads are cached by path, so a mutation reading the same blob twice fetches it once. `raw`/`tree` are unchecked: upgraders read older formats through them.
- `fan_path`: entry `n` at `dir/{n>>8:04x}/{n&255:02x}`, 256 entries per tree, so an append rewrites two small trees. `walk` and `count` read these numbered trees.
- `hash`: the sha1 blob id of a text, as a content hash with no repository (notes summaries, `files` tree).
- `edit_config`: a git config file rewritten under its lock (prov.rs's cached `ai.root`).
