# file.rs

Shared by memory, notes and chat.

- `pretty`, `AtPath`: paths as the user types them (`~/…`); I/O errors as `<path>: <strerror>.`.
- `try_lock`, `lock_before`, `lock`: advisory locks on a file (`File::try_lock`/`lock`), created if absent and released by the OS when the process dies, so a crash leaves no stale lock. Compactor locks (memory's `compact.lock`, notes' summariser lock, a chat thread's lock) and notes' write lock.
- `Lock` unlocks (`LOCK_UN`) on drop: a child forked by another thread holds a copy of the descriptor until it execs, and closing ours alone would leave the lock held through that copy.
- `lock_before` waits until a deadline by trying every 50 ms: std has no timed `flock`, and a blocking `lock` on a helper thread would outlive the timeout holding a pending lock. `try_lock` is `lock_before` now.
