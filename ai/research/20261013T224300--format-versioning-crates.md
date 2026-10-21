# Format versioning: crates, prior art, gix behaviour

Scope: versioning the `ai memory` store (bare git repo, `refs/ai/memory`, tree of text blobs), named header fields, chained tree upgraders, refusing newer stores, locking out published binaries 0.6.3 and 0.7.0.

Crate metadata from the crates.io API on 2026-10-13 (`/api/v1/crates/<name>`). Downloads are all-time / last 90 days.

## Summary

| Need | Recommendation |
|---|---|
| 1. Versioned types, chained migrations | Hand-written. Every surveyed crate versions individual serde records; our unit of versioning is the whole tree, and old decoders live only inside upgraders. |
| 2. Named header fields | Hand-written `key value` lines, blank line, body (git commit object layout). Around 40 lines. The candidate crates bring MIME, Debian or serde machinery we do not need. |
| 3. Prior art | Version marker in the data (tree blob `format`), absent marker means version 1, refuse newer with an "upgrade ai" message, upgrade in one commit with a CAS on the ref, frozen fixtures for every old version. |
| 4. gix and `repositoryformatversion` | gix 0.88/0.89 refuse version 2 and ignore unknown `extensions.*`. Plain git refuses both. Neither lever suits us (details below). Lock out old binaries from inside the data tree instead. |
| 5. Version type | `const FORMAT: u32` plus an upgrader array sized from it. Enums (cargo, rustup, gix 0.89) pay off when versions carry behaviour at runtime. Ours carry behaviour only inside upgraders. |

## 1. Versioned record crates

| Crate | Latest, released | Downloads | Licence | What it does | Fit |
|---|---|---|---|---|---|
| [obake](https://github.com/doctorn/obake) | 1.0.5, 2023-04 | 32k / 1.5k | MIT/Apache-2.0 | Proc macro generates one struct per semver version from `#[obake(cfg(..))]` fields. `From` impls between versions. `AnyVersion<T>` enum converts to the latest. | Per-struct, semver-keyed. Dormant since 2023. Old types stay in the main code, while our plan confines them to upgraders. |
| [versionize](https://github.com/firecracker-microvm/versionize) | 0.2.0, 2024-01 | 1.5M / 43k | Apache-2.0 | Firecracker snapshot format. Per-field `start/end` versions, a `VersionMap` that maps app versions to struct versions, bincode only. README: "experimental software outside the Firecracker context." | Binary only. No release since 2024-01. |
| [serde-version](https://github.com/fredpointzero/serde-version) | 0.5.1, 2019-11 | 15k / 11 | MIT/Apache-2.0 | Version-aware serde deserializer. | Abandoned. |
| [magic_migrate](https://github.com/schneems/magic_migrate) | 2.0.0, 2025-08 | 9k / 315 | Apache-2.0 | `#[derive(TryMigrate)]` with `#[try_migrate(from = V1)]`. Chains `TryFrom` impls and tries each version's deserializer in turn. Built for Heroku buildpack TOML caches, where data that cannot be loaded is discarded. | Closest in spirit (an explicit chain of `TryFrom`), but it guesses the version by trial deserialization. Our records have no serde form. |
| [native_model](https://github.com/vincent-herlemont/native_model) | 0.6.2, 2025-06 | 135k / 12k | MIT | Wraps serialized bytes in an (id, version) header. `from = DotV1` chains upgrades. Also supports downgrade for older readers. Bincode/postcard backends. Used by native_db. | Binary envelope per record. |
| [savefile](https://github.com/avl/savefile) | 0.20.5, 2026-07 | 199k / 13k | MIT/Apache-2.0 | Binary serializer with per-field `#[savefile_versions = "0..1"]` and schema checks. | Binary only. |
| [serde_versioned](https://github.com/taka-k0bayashi/serde_versioned) | 0.2.0, 2026-01 | 69 / 4 | MIT | `#[versioned(versions = [UserV1, UserV2])]` with `FromVersion`. | Unproven. |
| [serde-evolve](https://github.com/danieleades/serde-evolve) | 0.1.0, 2025-10 | 2.7k / 757 | MIT/Apache-2.0 | Representation DTOs `UserV1`, `UserV2` chained by `From`/`TryFrom` into a separate domain type. Derive `Versioned` with `chain(..)`. Adds a version tag to the serde output. | The cleanest design of the group: wire types kept apart from domain types. Single release. Still serde, per record. |
| [version-migrate](https://github.com/ynishi/version-migrate) | 0.21.0, 2026-07 | 8.8k / 2k | MIT/Apache-2.0 | `MigratesTo<Next>` + `IntoDomain<Domain>`, `migrator!("task" => [V1, V1_1, Entity])`, wrapped or flat `version` key, `Forwardable<T>` for unknown future versions, plus file and dir storage. | Large surface (storage, async, paths). Per record, serde JSON/TOML. |
| [pro-serde-versioned](https://github.com/ProspectiveCo/pro-serde-versioned) | 1.0.2, 2023-06 | 8.6k / 161 | Apache-2.0 | An enum of generations `V1(..), V2(..)` with `VersionedUpgrade` to the latest. | Dormant. Same per-record model. |
| [serde_flow](https://github.com/versolid/serde-flow) | 1.1.1, 2024-03 | 60k / 120 | MIT | `#[flow(variant = N)]`, `#[variants(V1, V2)]`, file migration, optional rkyv. | Dormant. File-per-entity, binary. |
| [rusqlite_migration](https://github.com/cljoly/rusqlite_migration) | 2.6.0, 2026-05 | 4.5M / 1.3M | Apache-2.0 | Ordered list of SQL migrations. Applied count stored in SQLite `user_version`. Errors with `DatabaseTooFarAhead` on newer databases. | SQLite only. Its model is the one to copy: an integer version equals the index into an ordered upgrade list, and newer stores are refused. |

Shared pattern: each version is a separate type; upgrades are `From`/`TryFrom` (or a trait method) from N to N+1; the latest version, or a separate domain type, is what the app uses. The crates differ only in how they detect the version (embedded tag, envelope header, trial parse) and in which serde backend they use.

Why none fits:
- They version single records. Our upgrade is tree to tree. It re-encodes every message blob, may repack segments and moves paths, so the natural shape is `fn(&Snapshot) -> Result<Changes>`.
- They require serde formats. Our records are hand-encoded text.
- They keep every old type compiled into the main data model. The plan confines old decoders to upgraders.

The chain is a few lines to write:

```rust
pub const FORMAT: u32 = 2;
type Upgrade = fn(&Snapshot) -> Result<Changes>;
const UPGRADES: [Upgrade; FORMAT as usize - 1] = [v1::to_v2];
```

`UPGRADES[v - 1]` takes version v to v+1. Raising `FORMAT` without adding an upgrader fails to compile.

## 2. Named header fields

Target layout (git commit object, RFC 822 header block):

```
ts 20261013T224300Z
kind note
origin 7KQ2MX
agent claude
model claude-opus-5-5
session 32dcb4ea-...

free text
```

Rules taken from git commits: `key SP value LF`; a blank line ends the header; keys are lowercase ASCII without spaces; the decoder rejects a missing required key and ignores unknown keys, which makes optional fields additive. gix keeps unknown commit headers as `CommitRef::extra_headers` (gix-object `lib.rs`). Drop empty fields instead of writing `-`. Values hold no newline: `field()` already folds whitespace, and git continuation lines (leading space) can be added later if a value ever needs one.

| Crate | Latest, released | Downloads | Licence | Notes |
|---|---|---|---|---|
| [mailparse](https://github.com/staktrace/mailparse) | 0.18.0, 2026-10 | 15M / 4.3M | 0BSD | MIME email. Header folding, RFC 2047 encoded words, charset handling. Parse only. |
| [mail-parser](https://github.com/stalwartlabs/mail-parser) | 0.11.9, 2026-09 | 4.3M / 1.8M | Apache-2.0/MIT | Full MIME parser (Stalwart). Parse only. |
| [rfc822-like](https://github.com/Kixunil/rfc822-like) | 0.2.3, 2024-09 | 57k / 8.6k | MITNFA | Serde (de)serializer for Debian control files. README: "API is currently not set in stone." Licence is non-standard. |
| [deb822-fast](https://github.com/jelmer/deb822-lossless) / [deb822-lossless](https://github.com/jelmer/debian-parsers) | 0.2.3 / 0.5.19, 2025-12 / 2026-09 | 104k / 226k | Apache-2.0 | Debian `Key: value` paragraphs. Lossless variant keeps formatting. |
| [serde-kv](https://github.com/a-poor/serde-kv) | 0.1.1, 2026-06 | 120 | Apache-2.0/MIT | `foo=bar baz=42` on one line. Unproven. |
| [toml](https://github.com/toml-rs/toml) | 1.1.8, 2026-10 | 1.0B / 256M | MIT/Apache-2.0 | Mature. Values must be quoted. Needs serde derive, plus a separator between header and body. |
| gix-object (already a dependency) | 0.65/0.66 | 60M | MIT/Apache-2.0 | Parses commit and tag headers only. No public parser for arbitrary header blocks. |

Verdict: write it by hand. The format is a closed set of ASCII keys with single-line values. The email crates handle encoded words, folding and charsets, and only parse. The serde formats add quoting, a serde dependency and derive code larger than the parser. A hand-written codec is about 40 lines, round-trips byte for byte, and is easy to fuzz with a proptest-style round-trip test.

Summary nodes: keep one record per line, since segment packing depends on it. Named fields within a line (`ts=.. origin=.. text`) buy little. The format version covers node-line changes, and nodes are cheap to rewrite in an upgrader.

## 3. How established tools version on-disk formats

| Tool | Where the version lives | Newer than the binary | Upgrade | Old readers after upgrade | Source |
|---|---|---|---|---|---|
| git | `core.repositoryformatversion` (0/1) in `config`. In v1, `extensions.*` keys act as feature flags. | Unknown version: "MUST NOT operate". Unknown extension in v1: "MUST NOT proceed". | Manual or implicit (e.g. `git init --object-format`). Guidance: avoid whole-repo bumps; version single files (index, pack, bitmap) or add data old readers ignore; read support first, opt-in write next, default last. | Fail closed through the version or extension. | [repository-version](https://github.com/git/git/blob/master/Documentation/technical/repository-version.adoc) |
| Mercurial | `.hg/requires` (and `.hg/store/requires` with share-safe): one feature name per line, no number. | `abort: requirement 'X' not supported!` | `hg debugupgraderepo`: builds the new store in a temp repo, writes the fake requirement `upgradeinprogress` to lock out every client during the swap, renames the store into place, then writes the final requires. The old store is kept as `upgradebackup.*`. | Fail closed: new requirements are unknown to old clients. | [RequiresFile](https://wiki.mercurial-scm.org/RequiresFile), [upgrade_utils/engine.py](https://repo.mercurial-scm.org/hg/file/tip/mercurial/upgrade_utils/engine.py) lines ~597-640 |
| SQLite apps | `PRAGMA user_version` (4-byte int at header offset 60) and `application_id` (offset 68; Fossil uses `'FSLG'`). | App-defined. rusqlite_migration: `DatabaseTooFarAhead`. | Each migration runs in a transaction, so it is all or nothing. | App-defined. | [SQLite file format](https://www.sqlite.org/fileformat2.html#database_header), [rusqlite_migration](https://github.com/cljoly/rusqlite_migration) |
| Fossil | `config` rows: `content-schema` (`"2"`, changes rarely, needs dedicated migration code) and `aux-schema` (a date; derived tables). | Aux schema outside `[MIN, MAX]`: `fossil_fatal("run \"fossil rebuild\" to fix this problem")`. | Content: special procedures. Aux: `fossil rebuild` regenerates derived tables from the artifacts. | Old Fossil rejects artifacts with new cards ("Older versions of Fossil will reject manifests that contain Q cards"). | [schema.c](https://fossil-scm.org/home/file/src/schema.c), [db.c](https://fossil-scm.org/home/file/src/db.c), [fileformat](https://fossil-scm.org/home/doc/trunk/www/fileformat.wiki) |
| Jujutsu | No repo-wide number. Op store is protobuf with additive fields. Index segments carry a `u32` format version. | Index: version mismatch triggers "Reindexing..." because the index is derived. | Op store: lazy, at read time (`has_git_refs_migrated_to_remote_tags` flags; legacy fields read when new ones are empty). It also dual-writes legacy fields (`bookmark_views_to_proto_legacy`) so older jj keeps reading. | Old jj keeps working on dual-written data. | [simple_op_store.rs](https://github.com/jj-vcs/jj/blob/main/lib/src/simple_op_store.rs), [default_index/store.rs](https://github.com/jj-vcs/jj/blob/main/lib/src/default_index/store.rs) |
| Cargo.lock | `version = N` at the top (since V3; V1/V2 are inferred from shape). | ``lock file version `N` was found, but this version of Cargo does not understand this lock file, perhaps Cargo needs to be updated?`` | Rewrites keep the existing version. New lockfiles pick the version from `rust-version`. Unstable versions are gated behind `-Znext-lockfile-bump`. | Fail closed through the number. | [src/resolver/resolve.rs](https://github.com/rust-lang/cargo/blob/master/src/resolver/resolve.rs) (`enum ResolveVersion`), [encode.rs](https://github.com/rust-lang/cargo/blob/master/src/resolver/encode.rs) |
| rustup | `version = "12"` in `settings.toml`. | `unknown metadata version: '{0}'` | Explicit `rustup self upgrade-data`. The V2 to V12 upgrade deletes the toolchains and update hashes, then writes the new version. | Fail closed through the parse. | [settings.rs](https://github.com/rust-lang/rustup/blob/main/src/settings.rs), [config.rs](https://github.com/rust-lang/rustup/blob/main/src/config.rs) `upgrade_data` |
| sccache | `CACHE_VERSION` bytes mixed into every cache key (`c.rs`: `b"13"`, `rust.rs`: `b"7"`). | Not applicable: a different version means a different key, so old entries become misses. | None. Old entries age out. | Unaffected. | [sccache src/compiler](https://github.com/mozilla/sccache/tree/main/src/compiler) |

Patterns that apply to us:
- **Where the version lives**: alongside the data it describes, read before anything else. Git keeps it in config because config is read first. For us the data tree is the unit that commits atomically, so a `format` blob at the tree root, versioned with every commit, is the analogue of SQLite's `user_version`.
- **Absent marker = oldest version** (git: absent means 0; cargo: no `version` key means V1/V2 by shape). Stores without `format` are v1.
- **Refuse newer**: an exact, actionable message naming the version found and what to run (cargo, rustup, Fossil).
- **Atomic upgrade**: Mercurial needs a lockout requirement and a rename because its store is many files. Git trees give atomicity for free: the upgrader builds the new tree and commits it on top of the old head, and the CAS on `refs/ai/memory` publishes it in one step. The old commit stays reachable in history as the backup. Do not move the ref.
- **Fail closed for old readers**: every tool except jj and sccache refuses on a marker the old binary already checks. Our 0.6.3 and 0.7.0 check no marker, so the lockout must be something they already trip over (§4).
- **Canonical vs derived** (Fossil content/aux, jj op store/index): derived data gets a version and is rebuilt; canonical data gets careful migration. Summary nodes are LLM output, so they are canonical, not rebuildable.
- **Fixtures**: cargo's `tests/testsuite/lockfile_compat.rs` keeps literal old lockfiles. jj tests legacy round trips (`test_remote_views_legacy_roundtrip`). For us: check in one frozen store per old format as plain files (`tests/fixtures/v1/log/0000/00`, `tree/1/0000/00`, ...) written once by v0.7.0. A test helper builds a git tree from the directory, opens it, runs the upgrade chain, and compares against expected v2 files. Plain files diff in review, and no binary repo is checked in.

## 4. gix and `core.repositoryformatversion`

Source: `gix/src/config/cache/incubate.rs`, `StageOne::new`, which runs on every `gix::open`.
- **gix 0.84.0**: reads the version only to decide whether to honour `extensions.objectFormat`. **Accepts any version** and ignores unknown extensions.
- **gix 0.88.0** (pinned by ai 0.6.3, 0.7.0 and HEAD; `Cargo.lock` resolves 0.88.0 at all three): `(version, _) => Err("Unsupported repository format version {version}; only versions 0 and 1 are supported")`. Applied in strict and lenient mode alike. `extensions.*` keys other than `objectFormat` and `worktreeConfig` are never read.
- **gix 0.89.0**: same behaviour through `Core::try_into_repository_format_version` -> `enum FormatVersion { V0, V1 }`.
- gix-discover 0.56 and gix-config 0.61 never check the version.

Empirical check: throwaway binary on `gix =0.88.0`, `default-features = false, features = ["anyhow","sha1"]` (same as ai), bare repos made with `git init --bare` + `git config`, git 2.55.0.

| Repo config | `gix::open` (0.88.0) | plain git |
|---|---|---|
| version 0 | OK | OK |
| version 0 + `extensions.aimemory=2` | OK | OK (git ignores unknown extensions in v0) |
| version 1 | OK | OK |
| version 1 + `extensions.aimemory=2` | **OK** (ignored) | `fatal: unknown repository extension found: aimemory` |
| version 2 | **ERR** "Repository configuration could not be loaded" / "Unsupported repository format version 2; only versions 0 and 1 are supported" | `fatal: Expected git repo version <= 1, found 2` |
| version 99 | ERR, same | fatal, same |
| version 1 + `extensions.objectFormat=sha256` | ERR `Invalid configuration value, "input"="sha256", "key"="extensions.objectFormat"` (sha1-only build) | OK |

What 0.6.3/0.7.0 do on a gix open error: `Store::find` maps any `gix::open` error to `None` (`let Ok(mut repo) = gix::open(dir) else { return Ok(None) }`). Every command then prints "No memory at ~/.ai/memory. To create one, run: ai memory init", and `init` refuses because the directory "is not a memory, and not empty". So the store fails closed, but with a misleading message.

Why neither git lever works:
- `repositoryformatversion = 2` locks out the new binary too: gix 0.88 and 0.89 hard-code the refusal before any open option applies, and plain git refuses as well (inspection and backup with `git` would break).
- `version 1 + extensions.<x>` does not stop the old binaries (gix ignores the key) and breaks plain git.

Lever that works: the data tree. Every data path in 0.6.3/0.7.0 goes through `Snapshot::log_len` (`Edit::new` for append and `put_node`, plus view, wake, zoom, grep and stats). `log_len` calls `find_tree` on the `log` entry. If the upgraded tree holds a **blob named `log`** (content e.g. `This memory is format 2. Upgrade ai: cargo install ai-tools.`) and messages live under a new path (e.g. `msg/`), each old command fails with gix's `Object named <id> was supposed to be of kind tree, but was kind blob.` (verified with gix 0.88 `find_tree` on a blob). Config-only commands keep working, and they are harmless. An old process already running when the upgrade lands loses its CAS, re-snapshots, and hits the same error. A second barrier: named-header messages fail the old `Message::decode` ("A message is corrupt."), and old `push` decodes message `i-1` on every append. Old `Node::decode` returns `None` on lines it cannot parse, treating them as unbuilt, so node-line changes alone do not stop an old binary. The `log` tombstone is the dependable lever.

From the version that introduces `format` on, newer stores are refused by the explicit check, and the tombstone does not need to be repeated.

## 5. A type for the format version

| Tool/crate | Representation |
|---|---|
| cargo | `enum ResolveVersion { V1..V5 }`, `Ord`, `default()` = current, `with_rust_version()` |
| rustup | `enum MetadataVersion { V2, V12 }` with serde rename, `FromStr`/`Display`, `#[default]` = current, `match` in `upgrade_data` |
| gix 0.89 | `enum FormatVersion { V0, V1 }`, parse refuses others |
| rusqlite_migration | `user_version` integer = number of applied migrations in a `Vec<M>` |
| jj index, Fossil, sccache | integer or string constants |
| obake, serde-evolve, native_model, version-migrate | one type per version (`FooV1`, `FooV2`), chained by `From`/`TryFrom`/`MigratesTo` |

Enums suit cases where the running code branches on the version (cargo writes several lockfile versions, rustup matches in `upgrade_data`). Our binary reads and writes only `FORMAT`. Older versions matter only as indices into the upgrade chain, and their decoders are private to each upgrader module (`upgrade/v1.rs` with its own `Message` struct). So: `pub const FORMAT: u32`, an `UPGRADES` array sized `FORMAT - 1` (compile-time completeness, as in rusqlite_migration), and one parse function for the `format` blob that returns `u32` and refuses `0` and anything above `FORMAT`. A `Format` newtype or enum would add a layer that no runtime code branches on.
