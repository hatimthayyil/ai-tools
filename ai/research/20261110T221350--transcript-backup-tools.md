# Transcript backup tools

Author: Claude Code (Claude Opus 5.5)

Which established backup tool keeps harness transcripts in place of `ai archive` (src/archive/), and what that lets ai-tools delete. Checked 2026-11-10 against: crates.io and GitHub release data, rustic issues and CI, nixpkgs at the config flake's pin, Home Manager master in the Nix store, rustic_core 0.13.0's source, restic's `doc/design.rst`, rustic's README, gitoxide's `crate-status.md`. Measurements were run in a throwaway scratch folder on generated data, then deleted.

## Requirements

1. No transcript byte is ever lost. Claude Code only appends (`cleanupPeriodDays` 99999); Codex and Claude can still delete or swap files (`/delete`, revert, `claude purge`).
2. The default store is a local folder; a remote S3-compatible store is optional config, never hardcoded.
3. A thread can get back the exact bytes it read, after the harness file is gone or rewritten.
4. Sandboxed agents, later, need bytes that sit on other machines; several machines may write.
5. SQLite harness databases (opencode.db 14.5 MB, Hermes state.db, Codex's SQLite files) are copied consistently while their harness writes them.
6. NixOS and Home Manager fit; licence compatible with ai-tools (CC0-1.0).

## Candidates

| | restic | rustic | kopia | borg | bupstash | plakar |
|---|---|---|---|---|---|---|
| Version, date | 0.19.1, 2026-07-05 | 0.11.4 / rustic_core 0.13.0, 2026-08 | 0.23.1, 2026-06-16 | 1.4.5 stable; 2.0.0b25 beta, 2026-09-27 | 0.12.0, 2022-11-07 | 1.1.8, 2026-10-20 |
| Language, licence | Go, BSD-2 | Rust, Apache-2.0 OR MIT | Go, Apache-2.0 | Python/C, BSD-3 | Rust, MIT | Go, ISC |
| nixpkgs | yes | yes | yes | 1.4.5 | yes | yes |
| NixOS / HM module | both | none | none | NixOS, borgmatic | none | none |
| S3 / R2 | yes | yes (opendal) | yes | 2.x beta only | no | yes |
| Chunking | Rabin CDC, 512 KiB–8 MiB, avg 1 MiB, fixed | Rabin CDC, size settable per repo | buzhash CDC, 1/4/8 MiB settable | buzhash CDC | CDC | CDC |
| Concurrent writers | backups share a lock; prune, forget, check exclusive | lock-free | lock-free (clock-based maintenance) | one writer | one | yes |
| Repo format | v2, stable since 0.14 | same as restic | own | own; 2.x new | own | own |
| Commits, all time; last 12 months (non-bot) | fd0 3,995, MichaelEischer 3,568; 484, Eischer 368 | aawsome 1,229, simonsan 188; core 106 + CLI 78, aawsome 61 + 46 | jkowalski 2,042, julio-lopez 475 | many | one, dormant (last push 2024-02) | company-backed, new |

Out: **borg** (1.x has no S3, 2.x is beta), **bupstash** (dormant), **plakar** (young, release candidates weekly, no module), **kopia** (sound and lock-free, but no Nix module and its policies live in the repository and are set imperatively, no gain over restic/rustic here). **Our own chunking on fastcdc + object_store** is what `src/archive` already is: it keeps us owning packing, indexes, encryption and repair. The decision rules it out.

restic and rustic share one repository format, so the choice between them is reversible. Verified: a repository rustic made with 64 KiB chunks reads back through `restic dump`, byte-identical, and `restic check` passes on it.

### Appended files: what a snapshot stores

Content-defined chunking stores only new chunks, but the file's last chunk changes with every append, so each snapshot stores that chunk again. A file below the minimum chunk size is stored whole each time. Measured: a file growing by appends, one snapshot after each of 50 appends (random base64 data, so compression helps little; real JSONL compresses several times better, the ratios hold):

| file | restic 0.19.1 (defaults) | rustic 0.11.4 (defaults) | rustic, chunks 16/64/256 KiB |
|---|---|---|---|
| 200 KiB → 540 KiB, +4 KiB each | 22.0 MB | 17.0 MB | 3.9 MB |
| 4 MiB → 7.9 MiB, +40 KiB each | 71.1 MB | 69.6 MB | 10.5 MB |

So the 1 MiB-piece problem reappears inside any CDC tool as tail churn: about one average chunk per changed file per snapshot. restic's chunk sizes are fixed; rustic sets them per repository (`rustic config --set-chunk-size 64KiB --set-chunk-min-size 16KiB --set-chunk-max-size 256KiB`), cutting churn 4–7×. Either way the cost is small in money: R2 is $0.015/GB-month with no egress fees. Estimate, unmeasured: five active transcripts, a snapshot every 15 minutes, restic defaults, about 0.1 GB a day of never-pruned churn.

Each backup run wrote 4 objects (data pack, tree pack, index, snapshot); restic also creates and deletes a lock. A 15-minute timer is about 400 writes a day, far inside R2's free Class A operations.

### Reading back

- restic: `restic dump <snapshot> <path>` writes one file to stdout; 1.0 s for 7.9 MiB from a local repository, index load included.
- rustic: `rustic dump <snapshot>:<path>`, 0.5 s. In process, rustic_core 0.13 has `node_from_snapshot_path`, `open_file` and `read_file_at(offset, length)` (src/repository.rs:1370–1435), so a range read needs no whole-file dump.
- Any key unlocks the repository's one master key (restic `doc/design.rst`); there are no read-only keys. A sandbox must not hold the repository key: `ai` outside the sandbox reads bytes and hands them in, as rule 8 of the storage note already says of the archive.

### Integrity and locks

- Both: client-side AES-256 + Poly1305, zstd, `check --read-data[-subset]`. `ai archive verify` becomes `restic check`.
- R2 bucket locks: restic must delete its `locks/` files, so a lock rule has to leave that prefix out (config research of 2026-10-08). rustic writes no lock files.
- Unverified: with nothing ever pruned, every run adds an index file. restic consolidates indexes only in `prune` / `repair index`, which delete old index files, and a bucket lock on `index/` forbids that. Needs a measurement of start-up time after thousands of runs before a lock rule covers `index/`.

### SQLite databases

- **Backup API, then back up the copy:** `ai backup` does this before each run (`src/backup/db.rs`): each database goes to its own path under `~/.ai/backup-staging/`, in one step of the API, and a copy the same as the last is left untouched. A page-for-page copy keeps page layout, so CDC deduplicates unchanged pages across snapshots. `VACUUM INTO` compacts and shifts pages, so it would deduplicate worse (reasoned, not measured).
- **litestream** (0.5.18, NixOS module): it takes over checkpointing and takes short write locks on the database. Fine for an app that runs it by design, intrusive on another program's database. Out.
- **sqlite3_rsync** (3.53.3): syncs a live database to a replica efficiently; it adds nothing once restic deduplicates pages. Out.

### Trigger

`ai backup watch`, a user service, runs every `[backup] every` (default 5 minutes); a session's end starts one more run from ai's end-session hook, once the repository is made. restic skips unchanged files by size, mtime and inode against the parent snapshot (documented; the run time over 1,155 transcripts / 832 MB is unmeasured). An inotify trigger would bring back `watch.rs` as our code, for a loss window only Codex `/delete` or `claude purge` within the interval could use.

A run with nothing new makes no snapshot: `ai backup` asks `restic backup --dry-run --json` first and backs up only when a file is new or changed. restic's own `--skip-if-unchanged` skips next to nothing here (measured, 0.19.1): a snapshot holds the metadata of every parent folder of its paths, and those change with anything written beside them.

### What a thread records

A thread reads live harness files as it does today; the backup is needed only once a file is gone or rewritten. A record holds `host`, `path`, `from`, `len` and `sha256` of the bytes read, and `at`, when they were read (`record::Pointer`). The bytes are read from the file while it begins with them, then from the local repository, then the remote one (`src/backup/read.rs`). The time finds the snapshot: the host's last snapshot before `at` or its first after holds the bytes if any does, so a read-back searches those two, a few later ones and the newest, never every snapshot (`restic find`, then `restic dump`). A snapshot id can't be recorded at sync time: the next backup hasn't run yet. A snapshot copied to the remote has an id of its own there, with the local snapshot's id as its `original` and the same `tree` (measured), so a snapshot id would also name only one of the two repositories. Piece hashes (`archive::read::Span`) go.

## What it lets us delete

| Layer | Lines | With restic/rustic |
|---|---|---|
| `src/archive/` (mod 957, files 982, read 170, verify 251, watch 207, homes 196, objects 150, db 240) | 3,153 | goes; `db.rs` may survive as the prepare step, or `sqlite3 .backup` replaces it |
| `tests/archive.rs` | 1,509 | goes |
| Step 2's glue: `source.rs` `Archived`/states/`everywhere`, `record.rs` `Pointer` pieces, `cli.rs` `unarchived`, sync's loss warnings | about 300 (estimate) | shrinks to path/len/hash records and one `dump` call |
| `nix/home.nix` archive service | about 45 | becomes a service that keeps `ai backup` running |
| `src/harness/` + `tests/harness.rs` | 1,686 | goes anyway (step 3) |
| `src/segments.rs` (1 MiB segments of `~/ai` thread files) | 281 | gone, apart from the backup: thread files are whole JSON Lines files, and `ai` starts `git gc` on `~/ai` once the loose objects written since the last pass `[agent] gc-after-mib`. gix writes packs without delta compression (crate-status: "delta compression [ ]"), so the `git` program does the packing |
| `src/agent/convert.rs` | 592 | not affected; its backup copy could become a restic snapshot of `.ai/local` before the move |
| Per-thread git repositories | — | already undone (storage note rule 4) |

About 5,000 lines of storage code and tests go, with every archive data-loss finding of the review.

## rustic maturity

Checked 2026-11-10 against rustic's and rustic_core's GitHub repositories (README history, issues, CI workflows, commit log), the rustic docs repository (`comparison-restic.md`, `breaking_changes.md`) and restic's `CHANGELOG.md`. The question: run as a program from `ai backup`, is rustic safe enough to replace restic? Library API churn and the missing Nix module no longer count, since `ai backup` runs the tool.

**Self-description.** Until 2026-06-15 the README said: "`rustic` currently is in **beta** state and misses regression tests. It is not recommended to use it for production backups, yet." Commit f6b46ec (#1783, aawsome) replaced that with "Should I use `rustic` or `restic`?": "The `restic` project is older and more mature than `rustic`, slow with new developments and conservative with changes", and "The best advise is to test both". The one release since is 0.11.4 (2026-08-18). The docs' comparison (restic 0.19.0 vs rustic 0.11.3) gives rustic test coverage as "(✅) (57% in rustic_core)" against restic's ✅.

**Data-loss and corruption reports** (rustic and rustic_core issues, by title search and the P-critical/P-high labels):

| Issue | Version, date | Cause | Outcome |
|---|---|---|---|
| rustic #88 | 2022-07 | prune repacking compressed blobs corrupted them | fixed same day |
| rustic #34 | 2022-06 | filename quoting in rustic-written trees; `restic check` failed on them | fixed 2022-09 |
| rustic #392 | 0.4, 2023-01 | prune deleted packs holding a needed blob more than once | fixed next day, 0.4.3 |
| rustic #753 | 2023-07 | zstd 1.5.4 corruption at high compression | fixed in 4 days; restic hit the same (#4523, #4677) |
| rustic #1313 | 0.9.2, 2024-10 | prune/check panic; nothing written | fixed next day, 0.9.3 |
| rustic #1946 (P-critical) | 0.11.4, 2026-10-15 | overlapping prunes with a forget between them delete packs a later backup deduplicates against | fixed by rustic_core PR 639, merged 2026-11-08; **unreleased** |
| rustic_core #490 | 2026-03-10 | "Blob ID is missing in index files" on prune | open, no reply in 8 months |
| rustic_core #583 | 2026-10-01 | a retried upload is trusted; the backend kept the truncated first attempt (pCloud via rclone) | open, no reply |
| rustic_core #612 | 0.11.4, 2026-10-25 | opendal's RetryLayer can skip a directory on a retried listing; `check` reports present packs missing, and `repair index` would act on it | open, no reply |
| rustic_core #647 | 0.13.0, 2026-11-06 | concurrent cache writes share one temp name and publish a corrupt cached file | open, no reply |
| rustic #1164, #635 | 0.7; Windows | flaky FUSE repository; Windows restores | open |

restic over the same years (0.14 → 0.19.1): the zstd bug and `repair packs` salvage gaps (#21820); no prune bug that deleted needed data.

None of rustic's past data-loss bugs would have hit a never-pruned backup: all sit in `prune`. The open ones partly would: #612 is in opendal, which rustic uses for S3/R2 and restic doesn't (restic uses minio-go); #583 is a write path both tools trust.

**Tests and releases.** rustic_core: unit tests, proptest in three places, integration tests per command (backup, check, prune, append_only, hotcold, dump, copy and more), tarpaulin coverage, CI on Linux/macOS/Windows plus eleven cross targets, cargo-careful, cargo-audit. rustic: CLI integration tests and a weekly compatibility job that makes a repository with the latest restic and checks it with rustic. The reverse, restic reading a rustic repository, isn't in CI. No fuzzing. release-plz drafts version PRs; binaries come from a separate release workflow.

**People, last 12 months** (since 2025-11-10, non-bot commits): rustic_core 106 (aawsome 61), rustic 78 (aawsome 46); restic 484 (Michael Eischer 368, fd0 9). Both are now mostly one person's work; restic has five times the activity. Issues opened in that time with no reply from anyone but the author: rustic 83 of 161, rustic_core 41 of 65, restic 46 of 198. Median first reply: 9.3, 3.8 and 5.1 days. rustic has no organisation or funding beyond aawsome's GitHub Sponsors.

**CLI and config stability.** Every minor release breaks options (`breaking_changes.md`): 0.9 (`password-command` syntax, copy, key names), 0.10 (renamed options), 0.11 (`parent` → `parents`, atime/devid options, duration syntax, logging; trees now written with restic's timestamp format), 0.12 unreleased (warm-up output). `ai backup` would generate rustic's options, so a nixpkgs bump could need an `ai` change.

**Features that matter here:**
- Lock-free: no lock files for any command. restic backups take a shared lock and also run from several machines at once; the gain is no stale locks, and an R2 bucket lock needs no `locks/` exception.
- Chunk sizes per repository: 4–7× less tail churn (measured above).
- In-repo `append_only`: refuses forget, prune and config, but the key holder can switch it off. A guard against mistakes, not a stolen key; the bucket lock stays the real guard.
- Hot/cold repositories (metadata in a hot one) serve cold storage classes; not needed for a local folder or R2.
- `dump <snapshot>:<path>` writes whole files, as restic's does; range reads exist only in rustic_core.
- Both: `password-command`, stdin backups, `check --read-data-subset`, S3.
- rustic has no `repair packs` or `recover`; restic can run them on a rustic repository.

**Format compatibility.** #34 (2022, fixed); the 0.11.0 timestamp change; #1671 (rustic `check` errors on a repository a restic `prune` was changing, closed as explained by that, not a format fault). Our test above read a rustic repository with restic, byte-identical.

**Verdict: rustic doesn't win yet.** What it adds (no locks, smaller chunks, config profiles) saves money and operations; none of it makes a byte safer. Against it: it called itself not ready for production backups five months ago, with one release since; a critical data-loss fix is unreleased; half its issues get no reply; R2 would go through opendal, which has an open listing bug restic doesn't share; and its options break every minor release. Revisit when the #1946 fix ships and #583/#612 are answered. Switching needs no migration, and `restic check` can verify a rustic repository afterwards.

## Recommendation

**restic, run as a program by `ai backup`; `ai` reads bytes back with `restic dump`.**

- Maturity matters most for "never lost". restic has the larger, more active project, a stable format and options that rarely change. The rustic section above weighs the alternative.
- Running a tool, not linking a library, keeps storage out of `ai`, which is the decision.
- restic's fixed chunk sizes cost tail churn, cheap at R2's prices. If it ever matters, rustic works on the same repository with smaller chunks: no migration.
- Shape, as built: `[backup]` in the user's own `ai.toml` (`repository`, a local folder, always there; `password-command`, else a key file `ai` makes; `every`; `on-session-end`; `folders` for extras). `ai` runs `password-command` and `[backup.remote] env-command` itself, under `sh -c`, and stops each after 60 seconds with everything it started, so a command that hangs fails its run and blocks no later one. `ai backup` passes the harness folders it knows; the Home Manager service only keeps `ai backup watch` running. With `[backup.remote]`, each run then copies the snapshots the remote lacks with `restic copy`; `ai` makes the remote with `init --copy-chunker-params`, so copies deduplicate as the local repository does.

The 2026-10-08 config research still holds on restic and R2, the timer, no pruning and the bucket lock (with the `locks/` and `index/` caveats above). It is outdated on rustic (now 0.11.4 with tunable chunks, no longer calling itself beta) and on borg 2 (still beta, now with S3).

## Open questions

1. **Index growth without prune:** measure start-up after a year's worth of runs before locking `index/` on R2.
2. **Records from another machine:** with `host` in each record, another machine's bytes come from the shared remote repository; without a remote they're unreachable, as the storage note's open question 6 says too.
3. **Snapshot count without forget:** measured (restic 0.19.1, local repository, 300 files; storage note, "Snapshot count"). Seconds at 1,564, 12,520 and 50,084 snapshots: a dry run that finds a change 0.6, 2.0, 5.6; the backup after it 1.1, 1.5, 3.5; a dry run that finds none 0.6, 1.5, 4.5; `find` in every snapshot for one path 6.8, 40, 134; `dump` of one file 0.9, 1.4, 4.2. Every command reads every snapshot's record first, about 0.09 s more per 1,000. A run every 5 minutes stays workable at 50,084 snapshots, half a year of a change every 5 minutes around the clock; a search of every snapshot does not, which is why records carry `at`: a read-back of one file took 9 s there. Only 1,564 of the snapshots came from real backups, so the cost of one index file per run (question 1) is still unmeasured.
