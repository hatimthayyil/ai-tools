# chat/sync.rs

Design: `ai/research/20261014T002322---chat-design.md`, Threads and sessions, Contract.

- `read`: the harness's adapter by name (`claude` only); the harness stays a string so pi and Codex add a match arm, not a type. A transcript not written yet (a session launched and bound before its first entry) reads as empty.
- One session lock (store.rs) is held through each attach, sync and detach of that session: its owner lookup, the capture of its files and every write. Syncs of one session therefore run one after another and each captures after the last finished, so an older observation never lands after a newer one. A detach landing first turns a sync into `Synced::NotAttached`.
- `sync`: no repository, or the session attached to no thread: `NotAttached`. Capture runs under the session lock and outside the thread's writer lock; the writes run under both, against the thread's files read again. A transcript path given that differs from the track's is recorded with the next track.
- `attach`: a different owner is rejected before any source read or write; the session must be detached first. The source path is the argument, else this thread's track for the session. A new track's `graft` is the thread's newest item; an existing one keeps its graft. The attachment event is appended last, after the thread's items and track, and only when the session was not attached here: reattaching to the same thread catches up without resetting its attach time, and unchanged data writes nothing.
- `detach`: catches up first (`catch_up`, the same ingest as sync, under the same session lock), so entries written since the last hook are kept; then appends a detached event. A failed catch-up still detaches and reports the failure: a broken transcript or thread never pins a session. The track stays, with its graft.
- `attached`: the owner and its binding; an owner whose thread has no track of the session is an error.

Regression tests
- Private `sync_with` and `attach_with` inject the transcript reader; public methods supply the existing adapter. An older sync holds its capture while the transcript grows and a newer sync waits on the session lock; the thread ends as a fresh attach of the whole transcript. Concurrent initial attaches start together; one succeeds and the other rejects the occupied session without making a thread.
- The source/projection test changes the live file immediately after capture. The archived bytes and parsed items must both describe the captured observation.
