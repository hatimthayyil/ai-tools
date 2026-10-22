# config.rs

`ai.toml`: every setting `ai` reads, one table per tool (`[chat]`, `[memory]`), keys kebab-case as Cargo's. Each table is its tool's own type (`chat::Config`, `memory::Config`), so a tool owns its keys, defaults and checks. A project's file also holds `[project]` (`project::Table`, project.rs.md).

- Two files: the user's in `home()` (`~/.ai/ai.toml`, from `$HOME`), and the project's, the nearest `ai.toml` at or above the cwd, found as Cargo finds `Cargo.toml`. The search skips the user's file (`nearest`, compared by path, then canonically), so a cwd under `~/.ai` is not in a project. Only the nearest project file counts. The project's value of a key wins over the user's; a key neither sets keeps the tool's default.
- `File` is one file as written, `[project]` included; `Settings` is what the two files set together. `[project]` in the user's file is an error.
- A missing file sets nothing. An unreadable one, a TOML error, an unknown table or key, or a bad value fails the command with the file's path before the TOML error, which shows the line and the key (`main` prints only the outermost message, so the path is part of it, not a context).
- Only `ai init` writes `ai.toml`, and only `[project] id` (project.rs).
- `Settings::find` is the CLI's; `Settings::load` takes the user's file as an argument, for tests.
- Who loads it: `ai memory` and `ai view` at the command's start (`memory::Runtime::config`), `ai chat` when a command needs its settings (`Chat::config`, from the directory the store was found from), and `Project::find` for `[project]`.
