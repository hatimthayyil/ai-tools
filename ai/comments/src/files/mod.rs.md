# files/mod.rs

`ai files`: file summaries rolled up the directory tree. `tree` walks the project, `store` keeps summaries by content hash, `summarise` writes them, `cli` joins the three. Re-exports the store and walk API so callers write `files::Store`.
