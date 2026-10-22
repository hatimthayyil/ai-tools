# cliff.toml

- Upgrade steps come from commits: a `docs(upgrade): …` commit's body is the release's "Upgrading" section, printed whole and first. The steps live in history, so a prepended release section and a full regeneration both carry them, and nobody edits CHANGELOG.md by hand.
- `<!-- 0 -->` sorts "Upgrading" before the other groups, which git-cliff orders by name; `striptags` drops it from the heading.
