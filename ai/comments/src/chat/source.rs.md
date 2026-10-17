# chat/source.rs

- Capture the main transcript and its companion directory as relative paths and bytes before parsing. Main messages, subagent messages and metadata use that captured set.
- Archive writes and derived items share one thread commit. Missing live files retain the last archived copy; changed copies retain earlier versions through Git parents.
- Symlinks and non-regular files return errors; capture never follows a link into external files. Source paths must be representable as UTF-8 Git paths.
