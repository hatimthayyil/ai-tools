# chat/sync.rs

Design: `ai/research/20261014T002322---chat-design.md`, Threads and sessions, Contract.

- `read`: the harness's adapter by name (`claude` only); the harness stays a string so pi and Codex add a match arm, not a type. A transcript not written yet (a session launched and bound before its first entry) reads as empty.
- `sync`: no repository, or no thread with the session attached: `Synced::NotAttached`. Inside the thread mutation, read the current binding and capture its source files, then derive items from those bytes. A lost compare-and-swap repeats capture against the new binding, so an older observation cannot replace newer progress. A detach landing first turns sync into `NotAttached`.
- Transcript path: the one given, else the binding's. A given path differing from the binding's is recorded with the next commit.
- `attach`: source path precedence is the given path, the session's previous thread, then this thread's current binding. Capture occurs inside the destination mutation and repeats on a lost compare-and-swap. A new binding's `graft` is the thread's newest item; an existing one keeps its graft. Cross-thread detach and attach still publish separately.
- One attached thread per session: `attach` detaches it elsewhere first. Two attaches racing on two threads can leave both attached; `attached` then takes the latest `attached` time, which is also the binding the later attach wrote.
- `detach`: marks the binding detached; no sync. The binding stays, with its graft.

Regression tests
- Private `sync_with` and `attach_with` inject the transcript reader; public methods supply the existing adapter. Channel gates hold an older observation until a newer sync publishes and hold two initial attachments before either publishes. Retries pass through the reader again.
- The source/projection test changes the live file immediately after capture. The archived bytes and parsed items must both describe the captured observation.
