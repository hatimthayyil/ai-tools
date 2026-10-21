# config.rs

`ai.toml`: every setting `ai` reads, one table per tool (`[chat]`, `[memory]`), keys kebab-case as Cargo's. Each table is its tool's own type (`chat::Config`, `memory::Config`), so a tool owns its keys, defaults and checks.

- Two files: the user's in `home()` (`~/.ai/ai.toml`, from `$HOME`), and the project's, the nearest `ai.toml` at or above the cwd, found as Cargo finds `Cargo.toml`. Only the nearest project file counts. The project's value of a key wins over the user's; a key neither sets keeps the tool's default.
- A missing file sets nothing. An unreadable one, a TOML error, an unknown table or key, or a bad value fails the command with the file's path before the TOML error, which shows the line and the key (`main` prints only the outermost message, so the path is part of it, not a context).
- `ai` only reads `ai.toml`; nothing writes it.
- `Settings::find` is the CLI's; `Settings::load` takes the user's file as an argument, for tests.
- Who loads it: `ai memory` and `ai view` at the command's start (`memory::Runtime::config`), `ai chat` when a command needs its settings (`Chat::config`, from the directory the store was found from).
