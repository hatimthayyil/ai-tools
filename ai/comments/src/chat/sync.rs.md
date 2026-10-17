# chat/sync.rs

Design: `ai/research/20261014T002322---chat-design.md`, Threads and sessions, Contract.

- `read`: the harness's adapter by name (`claude` only); the harness stays a string so pi and Codex add a match arm, not a type. A transcript not written yet (a session launched and bound before its first entry) reads as empty.
- `sync`: no repository, or no thread with the session attached: `Synced::NotAttached`. Inside the thread mutation, read the current binding and capture its source files, then derive items from those bytes. A lost compare-and-swap repeats capture against the new binding, so an older observation cannot replace newer progress. A detach landing first turns sync into `NotAttached`.
- Transcript path: the one given, else the binding's. A given path differing from the binding's is recorded with the next commit.
- `attach`: a different active owner is rejected before source reads or thread writes. The source path comes from the explicit argument, else this thread's binding for the same session. Capture occurs inside the destination mutation and repeats on a lost compare-and-swap. A new binding's `graft` is the thread's newest item; an existing one keeps its graft. Reattaching to the same active thread catches up without resetting its attach time; unchanged data writes no commit.
- One OS file lock per harness/session in the common Git directory serialises ownership lookup and attach/detach publication. Public `attached` scans use the same lock; internal locked operations use a private lookup to avoid recursive acquisition. Duplicate existing ownership is an error.
- `detach`: marks the binding detached; no sync. The binding stays, with its graft.

Regression tests
- Private `sync_with` and `attach_with` inject the transcript reader; public methods supply the existing adapter. A channel gate holds an older observation until a newer sync publishes. Concurrent initial attaches start together; one succeeds and the other must reject the occupied session. Retries pass through the reader again.
- The source/projection test changes the live file immediately after capture. The archived bytes and parsed items must both describe the captured observation.
