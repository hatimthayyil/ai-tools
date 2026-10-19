# chat/source.rs

- Capture the main transcript and its companion directory as relative paths and bytes before parsing. Main messages, subagent messages and metadata use that captured set.
- The archive (store.rs) is written from these bytes, under the same locks as the items derived from them. Missing live files retain the last archived copy.
- Symlinks and non-regular files return errors; capture never follows a link into external files. Source paths must be UTF-8.
- The main transcript is a regular file or absent. Only its companion directory is traversed recursively.
