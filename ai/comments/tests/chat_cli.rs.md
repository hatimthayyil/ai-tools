# tests/chat_cli.rs

`ai chat` as a binary. `Sandbox`: a temporary directory (canonical, as `getcwd` gives it) holding a repository (`gix::init`, background summariser off), a Claude Code config dir (`CLAUDE_CONFIG_DIR`, so transcripts go to `projects/<project dir of the repo>/`), `HOME`, and a fake `claude` first on `PATH` that writes its arguments to `out/argv` and its working directory to `out/cwd`. `GIT_CEILING_DIRECTORIES` stops discovery at the sandbox, so `out/` is outside any repository and a second repository (`init`) sits beside the first.

Lines are written through the library (`Thread::put_line`), keyed by `turn_ends`, as the roll-up would. `ran_in` stamps a transcript's entries with the `cwd` Claude Code records.

- Plumbing (`sync-session`, `compact-session`) of an unattached session: exit 3, no output, by flags or environment, and outside a repository.
- Porcelain without a thread (`view`, `zoom`, `summarise`; `detach`): a sentence saying what to do, in and out of a session; an unknown `-t` thread; outside a repository.
- Help lists the porcelain and hides the plumbing.
- Attach (transcript found from the session id), sync by flags, re-attach, moving threads, detach; errors without a session, with a bad name, outside a repository.
- `compact-session` syncs first, builds the free lines the fold needs, then prints the instruction, the fold (a written line, two free lines), the rail and the open turn; a session whose harness cannot be read: exit 1, the reason on stderr.
- `list`: threads with turns, newest time and branches; their sessions newest first, attached or detached, the calling one marked.
- `view` syncs its session and writes no line, its unsummarised turns shown by their messages; outside the session it reads without a sync; `compact-session` then writes the lines.
- `view` in three parts whose footers chain (`-t <name> --part --at`), joined equal to `compact-session`'s output, although a line is built between parts; in and out of the session alike.
- `zoom` of a node, a node with an unbuilt half, a turn, an item, a long item in two parts, by session and by `-t`; bad ids.
- `start`: argv and directory, binding with the future transcript path, graft of a second session; `view` of a thread with nothing yet. The seed: `compact-session` of a launched session prints nothing on a new thread, and the view ending at its graft on an existing one.
- `resume`: unknown thread; the newest session resumed and re-attached in the directory it ran in, with the launcher's args, also from a subdirectory; a session that ran outside the repository and one whose transcript is gone start a new session, with the reason on stderr and the same args.
- Two repositories, each with a `dev` thread: `list`, `zoom -t` and `resume` see only their own.
- `summarise`: a dry run with `--model` (in the thread's log) stores nothing and keeps a line written by hand; a write, then nothing to write; `--rebuild --dry-run` shows the line it replaces; `--rebuild` rewrites it.
