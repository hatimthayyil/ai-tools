# LadybugDB as a long-term dependency

Assessment of LadybugDB (Kuzu fork) for `ai`'s storage: code graphs, ontologies, rolled-up summaries, agent memory, multi-agent review. Primary sources: engine checkout `/hatimthayyil/code/ladybug` (HEAD `cf236ad`, 2026-10-12), Rust crate checkout `/hatimthayyil/code/ladybug-rust` (`28aba1c`, 2026-09-28), published crate `lbug-0.21.2`, docs/blog/site/extension repos cloned to `/hatimthayyil/tmp/db-survey/lb-*`, GitHub/crates.io/HN APIs. Date: 2026-10-12. No benchmarks (separate agent). Builds on `20261012T180851--storage-landscape.md`.

## Summary

- **Healthy velocity, one-person project.** 54 tags since the fork, ~3/month lately; 1,401 commits in 12 months. Arun Sharma wrote 74% of 12-month and 73% of 6-month commits, 69% of 6-month PRs, and 288 of ~480 issue comments. The next contributor has 5%. Bus factor 1. A small company, Ladybug Memory Inc., sponsors it. No public funding, revenue or team beyond him.
- **Responsive.** Median first reply to an issue is 3.3 h; the median issue closes in 1.3 days; outside PRs merge in 16 h median (135 of 156 merged).
- **Correctness churn is high.** In the last 6 months, 82 of 300 issues concern crashes, corruption, WAL or locks. Several were durable corruption: SIGKILL during COPY, WAL records over 2 KB unreplayable after a crash, a failed checkpoint corrupting rels, HNSW + CHECKPOINT heap corruption. Most are fixed; #843 (an unopenable DB) is still open. The v0.21 notes say "many improvements fix problems where previous releases silently returned wrong results."
- **Process model is unchanged in what matters.** There is one read-write process per file. Read-only processes can now open beside a writer, but they see the state as of open time. Reopening while a checkpoint runs fails, and the official docs still call it unsafe. Parallel agents need a server.
- **Extensions download at runtime.** FTS, vector, algo and JSON functions come from `INSTALL` against `https://extension.ladybugdb.com/`, with no signature check. The Rust crate cannot link them statically.
- **The Rust crate's default build fetches the *latest* GitHub release archive.** The archive is unpinned (not the crate version) and unchecksummed. It needs `LBUG_BUILD_FROM_SOURCE=1` and a C++ toolchain to be reproducible.
- **The storage format moves fast but reads forward.** Storage version went 40 → 49 in 5 months. Newer builds read every version back to 40 (0.12). Downgrade is gated on a successful `CHECKPOINT`.
- **"AI memory" is positioning, not yet a product.** The company site says "graph based continuous learning platform … coming soon". The actual memory code is `ladybug-memory`, a 60-commit Python library by one author last touched 2026-08-19, plus a Hermes plugin. Current focus is the GraphLake/Icebug/lakehouse work, not agent memory.
- **Verdict: do not adopt as the primary store.** At most, it is an optional derived read-mostly graph index behind `ai serve`, rebuilt from SQLite. Revisit only if SQL graph queries prove insufficient. Details below.

## 1. Project health

### Releases since the fork

Kuzu's last release was 0.11.3 (2025-10-10); the repo was archived the same month. The LadybugDB org and repo were created 2025-10-07.

| Period | Releases | Notes |
|---|---|---|
| 2025-11 → 2026-01 | 0.12.0 (11-03), 0.12.2, 0.13.0/.1 (12-15), 0.14.0/.1 | 0.12.0 = Kuzu 0.11.3 renamed; CI moved to GitHub runners |
| 2026-02 → 2026-05 | 0.15.0–0.15.4.2, 0.16.0/.1, 0.17.0/.1 | subgraphs, open-type graphs, Arrow/DuckDB/Parquet tables, Bugscope |
| 2026-06 → 2026-09 | 0.18.0–.3, 0.19.0/.1, 0.20.0–.4, 0.21.0–.2 (09-30) | ART index, optional hash PK, group commit, NaviX vectors, partitioning, GraphLake, SQL pushdown |
| next | 0.22.0 (storage v49 already in tree) | |

Minor releases come every 3–5 weeks, each followed by 1–4 quick patches. There is no LTS or 1.0 roadmap, and the site's JSON-LD claims `"softwareVersion": "1.0"`.

### Commits and contributors (`git shortlog -sne <since> HEAD`)

| Window | Commits | Distinct emails | Arun Sharma (2 identities + "CI Bot") | Next |
|---|---|---|---|---|
| 12 mo (since 2025-10-12) | 1,401 | 63 | 1,053 + 7 (76%) | Ally Heev 74, Aikins Laryea 25, Darek 23 |
| 6 mo (since 2026-04-12) | 871 | 49 | 634 (73%) | Ally Heev 43, Aikins Laryea 25, Darek 23 |

Monthly commits rose from 48 (2025-10) to 171 (2026-09). The original Kuzu authors (ziyi chen, Guodong Jin, andyfeng, Benjamin Winger, …) have no post-fork commits. `.github/CODEOWNERS` still names `@benjaminwinger` for storage and Rust. In the last 6 months, 37 commits carry `Co-Authored-By: Claude …` trailers (Opus 5.5, Fable 5, Opus 4.8). Many recent issues are long, agent-style reproducer reports.

Rust crate repo, 12 months: Arun Sharma 33 of 45 commits.

### GitHub responsiveness (last 400 issues and 600 PRs, via `gh`)

| Metric (issues/PRs opened since 2026-04-12) | Value |
|---|---|
| Issues opened / closed | 300 / 246 |
| Open issues, whole repo | 82 |
| First reply from someone other than the author | median 3.3 h, p75 12 h (130 issues with replies) |
| Time to close | median 1.3 days |
| PRs opened / merged | 506 / 471; Arun Sharma authored 350 |
| Outside PRs | 156, 135 merged, 20 closed unmerged, median 16 h to merge |
| Reviews | adsharma 77, aheev 26, zahariash 15 |
| Issue comments | adsharma 288; next 25 |

### Company, governance, licence

- **Company.** `LICENSE` reads "Copyright (c) 2022-2025 Kùzu Inc. / Copyright (c) 2025-2026 Ladybug Memory Inc." Arun Sharma's GitHub profile lists company "Ladybug Memory, Inc", San Francisco, bio "second time founder". The site footer says "Proudly sponsored by Ladybug Memory". It offers "Enterprise Contracts Available" and "Commercial support contracts", with no prices. [ladybugmem.ai](https://ladybugmem.ai) describes "A Graph based continuous learning platform" as "Coming Soon". No funding round, team page or customers were found. The site's "10+ enterprise customers" belong to Kuzu, the predecessor.
- **Governance.** The v0.12 post says: "By dropping the CLA and letting contributors retain copyright, we are signaling the intention of running the project as an open community project … After a period of sustained contributions, expect yourself to be added to the org as a collaborator." There is no GOVERNANCE or MAINTAINERS file. The only public org member is `aheev`. The second org, `Ladybug-Memory`, holds icebug, icebug-format, ladybug-memory, pgembed and others.
- **Licence.** MIT for engine, extensions, Rust crate, Python and Node bindings, and MCP server. Docs and blog are CC-BY-SA-4.0. Some org repos have no licence (ladybug-python, ladybug-nodejs, ladybug-wasm, bugscope).
- **Bundled `third_party/`:** all permissive.

  | Licence | Libraries |
  |---|---|
  | MIT | alp, cppjieba, spdlog, yyjson, miniz, pcg |
  | BSD | antlr4 runtime, re2, zstd, lz4, snappy |
  | Apache-2.0 | fastpfor, mbedtls 3.1.0, parquet, thrift, simsimd |
  | Other permissive | brotli, utf8proc, pybind11 |

  mbedtls 3.1.0 (2021) and httplib 0.14.2 are old. Static `liblbug.a` exports zstd/lz4/roaring symbols unprefixed, which collide with `zstd-sys` and others. The crate has an opt-in `LBUG_LOCALIZE_BUNDLED_SYMBOLS=1` workaround.
- **Packaging.** nixpkgs has `ladybugdb` at 0.17.1 (C++ CLI and lib, no extensions; maintainer hamidr), four minor releases behind.

## 2. People and stated direction

**Arun Sharma** (`adsharma`) wrote LadybugDB and nearly all of the Ladybug-Memory org repos. Publicly:

- He says he spent years on Linux kernel and memory-allocator work at Facebook, with jemalloc purging patches ([HN 2026-03-16](https://news.ycombinator.com/item?id=47404107)).
- He writes a blog at [adsharma.github.io](https://adsharma.github.io/). Posts include "Beating the CAP theorem for graphs" (2025-09), "Graph Archiving" (2025-10), "Volcano Iterator", and "LLMs and Databases" (2026-04-13). The last is a "database maximalist" essay: "LLMs are themselves a probabilistic graph database that won the hardware lottery". It does not discuss agent memory.
- His other projects include py2many, vpg ("PGlite for native code") and `pgembed` ("Embedded PostgreSQL for Agents", 51★).
- He is active on HN as "LadybugDB maintainer here". He states a Rust "north star", reached incrementally ([47481428](https://news.ycombinator.com/item?id=47481428)). On memory: "Ladybug by default allocates 80% of the physical memory to the buffer pool. You can limit it" ([47513210](https://news.ycombinator.com/item?id=47513210)). He also says LadybugDB "has revamped the Kuzu WAL design. It shouldn't be hard to build WAL based replication" ([49443437](https://news.ycombinator.com/item?id=49443437)).
- Project X account `@lbugdb`; LinkedIn `company/ladybugdb`. No conference talks found.

The other recurring contributors are Ally Heev (`aheev`, reviewer, Rust/CI), Darek (`zahariash`, binder/storage fixes) and Aikins Laryea (Vela Partners; their concurrent-write Kuzu fork was ported in). None has a public role statement.

Stated direction, from the blog ([ladybug-blog](https://github.com/LadybugDB/ladybug-blog)):

- **"Hello World"** (2025-10-23), **v0.12** (2025-11-05): continuity first ("A top concern for our users is continuity").
- **"Spreading its Wings"** (2026-03-01): many bindings, external tables, subgraphs.
- **"Flying Solo"** (2026-07-01):
  - Argues Kuzu and "lightly modified forks" are unsafe "in the age of Mythos".
  - Claims a now-"stable storage format" through a TypeSpec→C++ codegen ("upgrade in place").
  - Reports one fsync per commit, group commit (6k tx/s in one configuration), NaviX vector search as default, ART index, and an optional hash PK.
- **v0.21 "GraphLake"** (2026-09-30): correctness (TSAN/UBSAN clean), Iceberg/DuckLake/Icebug-Disk, SQL pushdown, `pg_ladybug`.
- **Site:** "DuckDB for graphs — the KuzuDB successor", "Ship agentic solutions 10x faster", "built for highly regulated industries".

**The "AI memory" vision, as shipped:**

- [`ladybug-memory`](https://github.com/Ladybug-Memory/ladybug-memory) (Python, MIT, 12★): `LadybugMemory("memory.lbdb").store/search`. Tables are `Memory`, `MemoryLink`, `Entity`, `MentionedIn`, `Coreference`, `Relations`, `LogicalChunk`, `DiscoveredSchemaType`. It uses BM25 via the FTS extension, HNSW via the vector extension, and optional GLiNER2 entity extraction. A design doc describes an "H-GLUE" hierarchical chunking scheme. Its queries build Cypher by string interpolation. All 60 commits are by Arun Sharma; the last is 2026-08-19 (0.1.8).
- [`hermes-memory-plugin`](https://github.com/Ladybug-Memory/hermes-memory-plugin) (24★): wraps it as a memory provider for Nous Hermes Agent. "Fully local … no hosted component".
- [`mcp-server-ladybug`](https://github.com/LadybugDB/mcp-server-ladybug): one raw `query` (Cypher) tool. `ladybug-skill`: an agent skill.
- **Partition routing** (`partition-routing-hook.md`, Rust bindings 2026-09-19): an opaque C hook so that a separate "distributed wrapper" can place partition subgraphs on remote hosts. The core stays embedded. This hints at a commercial distributed or cloud layer; nothing public implements it.
- Third-party: the Leanpub book [*LadybugDB for Edge Agent AI memory*](https://leanpub.com/ladybugdb) by Volodymyr Pavlyshyn, updated 2026-04-05, pitches "persistent, structured, queryable memory that lives inside your application".

No statement was found on the business model beyond support contracts and the "coming soon" platform. None was found on how the project survives if its sole maintainer stops.

## 3. Technical fit

### Concurrency (source: `storage_manager.cpp`, `file_handle.cpp`, `transaction_manager.cpp`, `wal_replayer.cpp`)

| Question | Answer |
|---|---|
| Writers across processes | **One.** A RW `Database` takes an `fcntl` `F_WRLCK` on the data file. A second RW open fails with "Could not set lock on file". |
| Readers across processes | Many `READ_ONLY` `Database`s are allowed. |
| RO open while another process writes | **Mechanically allowed since 0.16** (Vela port, 2026-03-26: RO opens skip the lock). An RO open fails with "Cannot open database in read-only mode while checkpoint recovery is pending … retry" if the writer is checkpointing. An RO handle replays the WAL into its own memory at open and never sees later writes, so it must reopen to refresh. The [docs](https://github.com/LadybugDB/ladybug-docs/blob/main/src/content/docs/concurrency.md) still say: "you cannot have both `READ_WRITE` and `READ_ONLY` database processes open … (in a safe manner)", and recommend "One API server process". GitNexus now relies on this mode and hit "checkpoint is in progress" races (GitNexus #3186, #3188). |
| Within one process | Many connections and threads. Default is one write transaction at a time, serializable, with MVCC snapshot reads. `enableMultiWrites` (opt-in, 2026-04) allows concurrent write transactions. Checkpoints are non-blocking for readers. |
| Lock-holder crash | The lock is released by the OS. The next open replays the WAL. |

For `ai`, parallel agent processes must go through one owning process (`ai serve`). An RO open per short-lived CLI call is possible for stale-tolerant reads, but it races checkpoints and contradicts the docs.

### Durability and WAL

- WAL plus shadow pages with a commit point at the WAL's `CHECKPOINT` record (`docs/checkpoint_recovery.md`). Auto-checkpoint at a 16 MiB WAL. WAL checksums are on.
- Default `throwOnWalReplayFailure=false` in C++: a torn tail is silently truncated "up to where the error occurred". The Rust `SystemConfig::default()` sets it **true**.
- Since 0.20, one fsync per commit with group commit.
- The recovery format changed again after 0.21.2 (checkpoint bundle v2). Older builds can silently misapply or truncate a v2 WAL: "**Do not delete [the shadow file]** … deleting them loses committed data"; downgrades require a successful `CHECKPOINT` first.
- Directory fsync is skipped where unsupported.
- New open issues: #1211 (2026-10-12) "Storage format, especially WAL, is host-endian and not portable"; #843 durable corruption making a DB unopenable (0.19.1).
- Recently fixed: #714 (WAL with records over 2 KB unreplayable after hard kill), #1159 (SIGKILL during COPY corrupts the DB), #1051, #940, #896, #924.
- The commit log for 2026-10-10/11 alone has six WAL/checkpoint/PK-index recovery fixes. Recovery code is being rewritten now.

### Storage format and upgrades

- `StorageVersionInfo`: v40 for 0.12–0.16.1, v41 (0.17), v42 (0.18), v43 (0.19), v47 (0.20–0.21.2), v49 (0.22.0).
- `canReadStorageVersion` accepts 40–49, so newer engines open older files and catalog fields are gated by version. Kuzu 0.11 databases migrate via `EXPORT/IMPORT DATABASE`, per the site.
- Older engines cannot open newer files.
- Release cadence means a format bump roughly every 1–2 minors, but forward-read compatibility has held since the fork.

### Memory defaults

- Buffer pool is 80% of available RAM (`DEFAULT_PHY_MEM_SIZE_RATIO_FOR_BM = 0.8`). It is lazy, but large per process.
- Address-space reservation (`maxDBSize`) is 8 TiB on 64-bit, which fails on small ARM boxes (GitNexus #785).
- Threads default to `hardware_concurrency()`.
- A short-lived CLI should set `buffer_pool_size`, `max_db_size` and `max_num_threads` explicitly.

### Query language, schema, transactions

- **Cypher.** Strongly typed: node and rel tables must be declared. Open-type `CREATE GRAPH g ANY` subgraphs exist as a separate universe, and multi-label support is only partial. The dialect differs from Neo4j, and downstream projects hit "unsupported Cypher syntax" and wrong plans (cognee #4365, #5474).
- **Schema evolution.** `ALTER TABLE` supports ADD/DROP column (with defaults), RENAME table/column, rel ADD/DROP `FROM … TO …`, and `SET SORTED BY`. There is no column type change.
- **Transactions.** `BEGIN TRANSACTION [READ ONLY]`, COMMIT/ROLLBACK, auto-commit otherwise, serializable.
- **Core features.** Var-length and shortest-path recursion (`src/function/gds`), JSON *type* (functions are in the JSON extension), Arrow/Parquet scans.

### Extensions

Official extensions: ADBC, HTTPFS, POSTGRES, DUCKDB, JSON, SQLITE, FTS, DELTA, ICEBERG, AZURE, UNITY_CATALOG, VECTOR, NEO4J, ALGO, LLM, PG_CLIENT, GQL. They live in a separate submodule repo (MIT).

- Loaded by `INSTALL x; LOAD x;`, which downloads a `.lbug_extension` shared library from `https://extension.ladybugdb.com/v<ver>/<platform>/…` into `~/.lbdb/extension/`. HTTPS only, no signature or hash check, then `dlopen`ed.
- Static linking exists at the CMake level (`EXTENSION_STATIC_LINK_LIST`), but the published Rust crate ships `lbug-src` **without** `extension/`, and `build.rs` passes no extension list. A Rust user therefore gets FTS, vector, PageRank/WCC/SCC/k-core and JSON functions only by runtime download.
- A DB whose catalog references an extension fails to open if the binary is missing (cognee #5736, 2026-10-12).
- For a nixpkgs binary this means network access at first use, or a separately packaged extension set plus a patched crate.

### Rust crate `lbug` 0.21.2

- **Shape.** About 4.5 kLOC Rust over a `cxx` bridge to C++. `unsafe` appears 46 times, mostly FFI plumbing, plus `unsafe impl Send/Sync` for `Database` and `Connection`. Roughly 95 `unwrap`/`expect`/`panic` sites, mostly in `value.rs`, `query_result.rs` and `partition_routing.rs`.
- **API.** `Connection<'db>` borrows `Database`; prepared statements; `interrupt`, `set_query_timeout`; Arrow export behind a feature flag. The API is plain and adequate.
- **Size and build.** The crate is 9.9 MB (45 MB unpacked), with the full engine source vendored minus extensions. It always links dynamic `libssl`/`libcrypto` and `libstdc++` (plus `-rdynamic` for extensions) and whole-archives `liblbug.a`.
- **`build.rs` order:**
  1. `LBUG_LIBRARY_DIR` + `LBUG_INCLUDE_DIR` (external lib), else
  2. **download a prebuilt static archive.** It runs `scripts/download-liblbug.sh`, which resolves `releases/latest` from the GitHub API unless `LBUG_VERSION` is set. The crate version and the linked engine can therefore differ. The download is unchecksummed. Linux has a `compat` or `perf` variant. If that fails,
  3. build vendored source with CMake (needs cmake, a C++20 compiler and python3 for the single-file header). If `lbug-src` is absent, it curls a tarball from GitHub `main`.
- **For nixpkgs.** The sandbox has no network, so step 2 fails and it falls back to step 3. Set `LBUG_BUILD_FROM_SOURCE=1` to be explicit and deterministic, or point `LBUG_LIBRARY_DIR` at the `ladybugdb` derivation's `lib` output (version must match). Expect a multi-minute C++ build and a binary in the tens of MB (to be measured by the benchmark agent).
- **Usage.** 1.16 M all-time crates.io downloads, but per-version downloads over the last 90 days sum to about 9.4 k. 17 reverse dependencies, all small (cognee-graph, infigraph, rudof, codenexus, …).

## 4. Who uses it for memory and code graphs

| Project | Use | Experience from their issues |
|---|---|---|
| **GitNexus** (PolyForm NC, ~48k★) | primary store: code graph, FTS, vectors | #1488: RW lock broke multiple Claude Code MCP instances; fixed by making MCP read-only and analyze the single writer. #3186/#3188: "checkpoint is in progress" on RO reopen, plus a native regression 0.18.3→0.19.0. #785: 8 TB mmap on Raspberry Pi. #1424, #2233, #3270: SIGSEGVs on macOS arm64, WSL and Node. #3218: Windows FTS DLLs. #2441: native load failure reported as success. |
| **Cognee** (Apache-2.0) | default graph store (Kuzu→Ladybug), with SQLite + LanceDB | #5221: `csr_node_group.cpp:434 UNREACHABLE_CODE` in 13/20 batches. #4348: after SIGKILL a corrupt WAL left writes failing silently for two days, then the DB would not open. #4394: process-local lock violated PK uniqueness. #5541: "Could not set lock" after LRU reopen. #5736: upgrade fails, JSON extension missing. #5474: comma-joined MATCH planned as cross product. #5083: deadlock. Docs recommend Neo4j/FalkorDB for multi-agent use. |
| Graphiti (Zep) | Kuzu driver deprecated and slated for removal (previous note); no Ladybug driver found | |
| Mem0 OSS | `graph_store` lists Kuzu; hosted product dropped external graph DBs | |
| Ladybug-Memory, Hermes plugin | own memory layer | single author, small |
| Rust: infigraph, codenexus, rudof, cognee-graph | small crates | |

The pattern: both large adopters run Ladybug inside an indexer or service that owns writes, and both spent much effort on lock, checkpoint and native-crash handling.

## 5. Risks

| Risk | Severity for `ai` | Note |
|---|---|---|
| Bus factor 1, plus a pre-revenue company | high | Arun Sharma does ~75% of the work across 43 + 20 repos (Ladybug, Icebug, pgembed, vpg, bindings in 9+ languages). Attention is split. A pivot or burnout would recreate Kuzu's archival. |
| Correctness and durability churn | high | Durable-corruption bugs were fixed monthly through 2026-10. Recovery format v2 is landing now. Being an "agent-memory" store means many SIGKILLed short-lived processes, exactly the failure mode in #714, #1159 and cognee #4348. |
| Process model | high | One writer process. RO-beside-writer is undocumented and racy. Requires `ai serve` for any parallel use. |
| Runtime extension download | high for nixpkgs | FTS, vector, algo and JSON are fetched unsigned at runtime. They cannot be statically linked via the crate. |
| Crate build fetches latest prebuilt | medium | Fixable with env vars, but shows supply-chain looseness. |
| Format churn | medium | Forward-read so far. No downgrade. Host-endian WAL (#1211). |
| C++ in a Rust binary | medium | Build time, size, symbol clashes, libstdc++/openssl linkage, crash-not-error (assertions abort). |
| Cypher dialect | low–medium | Typed Cypher is fine for agents with the schema in context. Downstream projects did hit dialect and planner gaps. |
| Licence | low | MIT throughout; bundled deps permissive. |

## Best way to use it, if adopted

1. **Not as the sole store.** Operational state (memory notes, review threads, claims, summary cache keyed by blob/tree oid) stays in SQLite WAL, which has multi-process writes and no daemon.
2. **As a derived, rebuildable graph index**, owned by one process:
   - `ai serve` (or the indexer) is the single RW owner and builds from SQLite and git.
   - Clients query over a local socket.
   - Or the owner builds a fresh file and atomically renames it, and short-lived CLIs open the finished file read-only. This sidesteps checkpoint races, because nothing writes that file after the rename.
   - Losing the file costs a rebuild, never data.
3. Pin the engine (`LBUG_BUILD_FROM_SOURCE=1` or `LBUG_LIBRARY_DIR` from nixpkgs). Set `buffer_pool_size`, `max_db_size` and `max_num_threads`, and keep `throw_on_wal_replay_failure=true`.
4. Avoid extensions. Use core Cypher and path functions. Keep FTS and vectors in SQLite (FTS5, sqlite-vec). If FTS or vector inside Ladybug becomes essential, package the extension binaries in Nix and point `LOAD` at local paths. Never use `INSTALL` from the network.
5. Expose task tools (callers, impact, subtree) over a fixed schema rather than raw Cypher, as GitNexus does.

## Verdict

**Do not adopt LadybugDB now, as the foundation or the sole store.** The engine is fast and actively fixed, and the maintainer is responsive. But the project is one person sponsored by a pre-product company, durability bugs are still being found monthly, and the process model needs a server. Its search and vector features arrive as unsigned runtime downloads that a nixpkgs build cannot carry. SQLite meets the current requirements without these costs. Keep LadybugDB as the designated option for a **derived, read-mostly graph index behind `ai serve`**, to be added only when measured SQL graph queries (recursive CTE or app-side BFS) prove inadequate.

Re-check triggers:

- a second maintainer with sustained commits (more than 15% share for 6 months);
- three consecutive minors with no durable-corruption bug;
- documented, tested RO-beside-RW support;
- statically linkable FTS and vector through the crate;
- a 1.0 with a format-stability promise.

## Open questions

- Is the RO-beside-writer mode intended as supported? A question to the maintainer, or watch for a docs change.
- Will the crate pin its prebuilt archive to `CARGO_PKG_VERSION` and verify checksums? A small upstream PR would settle it.
- Can `EXTENSION_STATIC_LINK_LIST` be exposed through `build.rs` (feature flags `fts`, `vector`, `algo`)? This is the precondition for any nixpkgs use of those features.
- What is Ladybug Memory Inc.'s funding and runway, and what is the "continuous learning platform"? Nothing public was found.
- Cost of the vendored source build under Nix: time, binary size, RSS at open with explicit limits. This is for the benchmark agent.
- Does the partition-routing hook signal a closed distributed product, with the embedded core becoming its on-ramp?

## Sources

- Engine: `/hatimthayyil/code/ladybug` — `LICENSE`, `third_party/versions.txt`, `src/include/main/database.h`, `src/main/database.cpp`, `src/storage/storage_manager.cpp`, `src/storage/file_handle.cpp`, `src/storage/checkpointer.cpp`, `src/storage/wal/wal_replayer.cpp`, `src/transaction/transaction_manager.cpp`, `src/include/storage/storage_version_info.h`, `src/include/extension/extension.h`, `src/extension/extension_entries.cpp`, `docs/checkpoint_recovery.md`, `partition-routing-hook.md`, `.github/CODEOWNERS`.
- Crate: `/hatimthayyil/code/ladybug-rust` (`build.rs`, `scripts/download-liblbug.sh`, `src/database.rs`) and crates.io [lbug 0.21.2](https://crates.io/crates/lbug).
- [LadybugDB/ladybug](https://github.com/LadybugDB/ladybug), [docs: concurrency](https://github.com/LadybugDB/ladybug-docs/blob/main/src/content/docs/concurrency.md), [blog](https://github.com/LadybugDB/ladybug-blog), [extensions](https://github.com/LadybugDB/extensions), [ladybugdb.com](https://ladybugdb.com), [ladybugmem.ai](https://ladybugmem.ai), [Ladybug-Memory org](https://github.com/Ladybug-Memory), [ladybug-memory](https://github.com/Ladybug-Memory/ladybug-memory), [hermes-memory-plugin](https://github.com/Ladybug-Memory/hermes-memory-plugin), [mcp-server-ladybug](https://github.com/LadybugDB/mcp-server-ladybug).
- [adsharma.github.io](https://adsharma.github.io/), [LLMs and Databases](https://adsharma.github.io/llms-and-databases/). HN: [Flying Solo](https://news.ycombinator.com/item?id=48751821), [47404107](https://news.ycombinator.com/item?id=47404107), [47481428](https://news.ycombinator.com/item?id=47481428), [47513210](https://news.ycombinator.com/item?id=47513210), [49443437](https://news.ycombinator.com/item?id=49443437). [Leanpub book](https://leanpub.com/ladybugdb).
- Downstream issues: GitNexus [#1488](https://github.com/abhigyanpatwari/GitNexus/issues/1488), [#785](https://github.com/abhigyanpatwari/GitNexus/issues/785), [#3188](https://github.com/abhigyanpatwari/GitNexus/issues/3188); cognee [#4348](https://github.com/topoteretes/cognee/issues/4348), [#5221](https://github.com/topoteretes/cognee/issues/5221), [#5736](https://github.com/topoteretes/cognee/issues/5736), [#5474](https://github.com/topoteretes/cognee/issues/5474); ladybug [#843](https://github.com/LadybugDB/ladybug/issues/843), [#1211](https://github.com/LadybugDB/ladybug/issues/1211), [#714](https://github.com/LadybugDB/ladybug/issues/714), [#1159](https://github.com/LadybugDB/ladybug/issues/1159).
- nixpkgs [`ladybugdb/package.nix`](https://github.com/NixOS/nixpkgs/blob/master/pkgs/by-name/la/ladybugdb/package.nix) (0.17.1).
