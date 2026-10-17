# chat/sync.rs

Design: `ai/research/20261014T002322---chat-design.md`, Threads and sessions, Contract.

- `read`: the harness's adapter by name (`claude` only); the harness stays a string so pi and Codex add a match arm, not a type. A transcript not written yet (a session launched and bound before its first entry) reads as empty.
- `sync`: no repository, or no thread with the session attached: `Synced::NotAttached`, after listing the refs under `refs/ai/chat/` and reading one binding blob per thread (0.3 ms). The plugin runs it at every turn end of every session. Attached: the transcript is parsed once, outside the mutation; the mutation (redone on a lost race) adds the entries the thread lacks and the binding's new `tip` in one commit, or commits nothing when nothing is new. A detach landing first turns it into `NotAttached`.
- Transcript path: the one given, else the binding's. A given path differing from the binding's is recorded with the next commit.
- `attach`: reads the transcript first (the given path, else the one the session had in its current thread, else in this one), then detaches the session from any other thread, then binds and ingests in one commit. A new binding's `graft` is the thread's newest item; an existing one keeps its graft, so re-attaching catches up from the transcript with every entry hung where it was.
- One attached thread per session: `attach` detaches it elsewhere first. Two attaches racing on two threads can leave both attached; `attached` then takes the latest `attached` time, which is also the binding the later attach wrote.
- `detach`: marks the binding detached; no sync. The binding stays, with its graft.

Regression tests
- Private `sync_with` and `attach_with` inject the transcript reader; public methods supply the existing adapter. Channel gates hold an older observation until a newer sync publishes and hold two initial attachments before either publishes. Retries pass through the reader again.
- The source/projection test changes the live file immediately after capture. The archived bytes and parsed items must both describe the captured observation.
