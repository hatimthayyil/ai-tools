# LadybugDB vs SQLite vs SurrealDB: measured

Adds LadybugDB (Kuzu fork) to the benchmark in `20261012T185452--db-benchmark.md`. Same machine, toolchain, deterministic dataset (`common/src/lib.rs`, unchanged) and queries; result counts and BM25 top hits match the other two engines on every query. Bench source: `/hatimthayyil/tmp/db-bench` (`bench-ladybug/`; targets and data deleted; raw output in `results/`).

## Summary

- **Queries: between SQLite and SurrealDB.** Most graph/lookup queries are 2–100× behind SQLite, mostly because every query costs ≥0.1–0.2 ms and a single-hop neighbour fetch plans a hash join over the whole node table (~2 ms). Deep traversals are close to SQLite and beat SurrealDB 5–8×: unbounded callers 13.5 ms (SQLite 7.5, Surreal 63); core-symbol callers 61 ms (SQLite 72, Surreal 476).
- **FTS (BM25) and HNSW both work and are fast.** BM25 is 3.5–4 ms, flat across queries (SQLite 0.5–1 ms, Surreal 2.4–41 ms). HNSW top-10 takes 2.7 ms at recall 1.000 (efs=40), the same speed as SurrealDB's at higher recall. The exact scan is 18 ms, against sqlite-vec's 31 ms.
- **Open is 56 ms** with a 64 MB buffer pool and 118 ms with the default pool (SQLite 1.7 ms, SurrealDB 679 ms). That is fine for a server and marginal for a CLI that opens the store per call.
- **Bulk load is fast for the graph** (0.63 s, 2× faster than SQLite) but **17.6 s in total**, because the HNSW build takes 13.0 s. The store is 102 MB (SQLite 79 MB, Surreal 628 MB). Peak RSS is 1.06 GB with the default pool. **With a 256 MB pool, COPYing the 61 MB of vectors fails** ("buffer pool is full").
- **Incremental update is ~29 ms per file**, the same as SurrealDB (27 ms) and 4× SQLite (6.7 ms). Single-row autocommit costs 4.2 ms. It takes the right Cypher idiom: the obvious `UNWIND … MATCH (a {id: e.a}), (b {id: e.b})` plans a 50k×50k cross product and costs 190–230 ms per statement.
- **Concurrency: one read-write process.** A second read-write open fails on the file lock, even a reader. A `read_only` open succeeds alongside a writer, but sees a snapshot frozen at open.
- **Build:** the prebuilt archive gives 66 s / 27 MB, but only with a GCC 13 C++ compiler; the host clang 21 (and GCC 15) shim segfaults at open. From source with fts+vector linked statically: **14–15.5 min wall / 153–162 CPU-min (two clean runs), 19.5 MB**. That needs a patched CMake list plus extra link args, because the crate does not wire static extensions.
- **Extensions:** the prebuilt archive has no fts/vector. They are downloaded from `extension.ladybugdb.com` at runtime (`INSTALL`) into `~/.lbdb` and `dlopen`ed on each process (+11 ms per open). Static linking works from source and removes all runtime network.
- **On-disk format:** since 0.17.0, newer releases open older files in place (storage versions 40–48), forward only. Before 0.17, any version change meant export/import. The format changed 7 times between 0.17 and the unreleased 0.22.
- **The Nix sandbox build succeeds** from source (`buildRustPackage`, no network, 14 min) and runs fts and vector offline. It needs three local patches: the static-extension CMake list, the extension link args, and `-include cstdint` for GCC 16.

## Setup

| | |
|---|---|
| Machine, toolchain, dataset, durability defaults | As in `20261012T185452--db-benchmark.md` (Xeon E-2176M, 12 threads, rustc 1.99.0, release, `strip = true`) |
| LadybugDB | crate `lbug` **0.21.2** from crates.io (= latest engine release v0.21.2, 2026-10-01; storage version 47), feature `arrow` (arrow 55.2). The local checkouts (`ladybug` v0.21.2-117, `ladybug-rust` with a stale 0.18.0 version field) were not used. |
| Extensions | fts + vector from the `ladybugdb/extensions` submodule pin `bf74ab6` (v0.21.2). Results below are from the **from-source build with both statically linked**; the prebuilt build gives the same query numbers within noise (it was used first). |
| Config | `SystemConfig::default()` (buffer pool = 80% of RAM, all cores, compression, checksums, auto-checkpoint) unless noted. `CALL var_length_extend_max_depth=1000` (default cap 30; the core-symbol caller set needs depth 31). |

Schema (Cypher):

- Node tables: `Dir(id PK, path, hash, summary)`, `File(id PK, path, hash, summary)`, `Symbol(id PK, name, kind)`, `Emb(id PK, v FLOAT[768])` and `Comment(id PK, thread, ts, author, body)`.
- Rel tables: `HAS_DIR(Dir→Dir)`, `HAS_FILE(Dir→File)`, `CONTAINS(File→Symbol)`, `CALLS(Symbol→Symbol)` and `IMPORTS(File→File)`.
- FTS: `CREATE_FTS_INDEX('File','summary_fts',['summary'], stemmer := 'none')`.
- HNSW: `CREATE_VECTOR_INDEX('Emb','emb_hnsw','v', metric := 'cosine')`, with defaults mu=30, ml=60, efc=200.
- There are no secondary indexes (primary key only), so the comment thread is a property scan.

Loading:

- Each table is registered as an Arrow table (`create_arrow_table`), then copied with `COPY T FROM (MATCH (x:arrow_t) RETURN …)`, then dropped.
- Indexes are built after the load.
- Comments are inserted one prepared `CREATE` per transaction.

## Results (three-way)

### Cost of having it

| | SQLite | SurrealDB 3.3.3 | LadybugDB 0.21.2 |
|---|---|---|---|
| Clean release build (wall / CPU) | 33 s / 45 s | 7 m 27 s / 65 min | **prebuilt: 66 s / 8.8 min**; **source + fts,vector static: 14 m 07 s / 153 min and 15 m 32 s / 162 min (two clean runs)** |
| Crates (normal + build) | 17 | 397 | 87 (60+ of them arrow, for bulk load) |
| Binary, stripped (`.text`) | 2.6 MB (2.1) | 108.5 MB (82.6) | prebuilt: 27.4 MB (20.2), with `-rdynamic` for dlopen'ed extensions; source+static ext: **19.5 MB (14.8)** |
| Open + point query, own process, median of 10 | 1.7 ms | 679 ms (38 ms in PID ns) | **118 ms** default pool; **56.7 ms** pool 64 MB; 46.3 ms read_only; 67.7 ms prebuilt + `LOAD fts, vector` |
| RSS of that process | 4.7 MB | 249 MB | 164 MB default; **62 MB** pool 64 MB |
| On-disk size after load | 79 MB | 628 MB | **102 MB** (single file `db.lbug`) |
| Peak RSS during load (incl. ~76 MB dataset) | 86 MB | 2.56 GB | 1.06 GB default; 0.95 GB pool 512 MB; pool 256 MB **fails** |
| Peak RSS during query run | 85 MB | 1.39 GB | 420 MB default; 290 MB pool 64 MB |

Where the LadybugDB open goes (pool 64 MB): `Database::new` 25 ms (reads ~2,300 pages = 9.5 MB), first query 15 ms, close 11 ms (it writes and fdatasyncs the header even with no writes). A default-sized pool adds ~47 ms of reservation.

### Bulk load (durable)

| Phase | SQLite | SurrealDB | LadybugDB |
|---|---|---|---|
| Open new store | ~1 ms | 847 ms | 50 ms |
| Graph: dirs, files, symbols, 250k edges (+ dir/file edges) | 1.35 s | 10.5 s | **0.63 s** (COPY from Arrow) |
| Summaries + FTS index | 0.16 s | 1.24 s | 1.58 s (index build; text loaded with File) |
| 20k × 768 vectors + index | 3.75 s (no ANN) | 17.8 s (HNSW 12.7 s) | 15.4 s (COPY 2.4 s + **HNSW 13.0 s**) |
| **Total** | **5.3 s** | **29.6 s** | **17.6 s** |
| 1k comments, one txn each | 3.31 s | 3.22 s | 4.10 s (4.1 ms/commit) |

### Queries, warm, median latency

The harness is the same (2 warm-ups, then up to 2 s or 50 samples). LadybugDB uses prepared statements with parameters.

| Query | Result | SQLite | SurrealDB | LadybugDB | LB / SQLite |
|---|---|---|---|---|---|
| Callers, depth ≤ 3 (`-[:CALLS*1..3]->`, DISTINCT) | 12 | 0.016 ms | 0.152 ms | 2.6 ms | 160× |
| Callers, unbounded (`-[:CALLS* SHORTEST 1..1000]->`) | 5,052 | 7.5 ms | 63 ms | 13.5 ms | 1.8× |
| Callers, unbounded, core symbol | 35,683 | 72 ms | 476 ms | **61 ms** | 0.85× |
| Shortest path (`p = (a)-[:CALLS* SHORTEST]->(b)`) | 7 hops | 4.3 ms | 6.2 ms | 10.8 ms | 2.5× |
| One file: symbols, in/out calls, in/out imports (5 stmts) | 87 rows | 0.086 ms | 1.24 ms | 11.3 ms | 130× |
| Top-50 by fan-in | (0, 3731) | 15 ms | 271 ms | 14.6 ms | 1.0× |
| Stale summaries: 5k list param, `UNWIND` + match on path | 250 | 4.0 ms | 392 ms | 273 ms | 68× |
| — 5k MAP param, `map_extract` per row | | | | 154 ms | |
| — fetch all (path, hash), diff in Rust | | | ~15–20 ms | **2.5 ms** | 0.6× |
| Dir rollup subtree (`-[:HAS_DIR*0..10]->`, 1 KB texts) | 73 | 0.071 ms | 3.4 ms | 1.9 ms | 26× |
| BM25 top-10 `cache` | 10, first 4556 | 0.96 ms | 41 ms | 4.0 ms | 4.2× |
| BM25 top-10 `parser token` (conjunctive) | 10, first 345 | 0.50 ms | 8.7 ms | 3.5 ms | 7× |
| BM25 top-10 `graph index commit` | 10, first 2555 | 0.53 ms | 2.4 ms | 3.5 ms | 6.6× |
| kNN top-10, HNSW efs=40 | recall | — | 2.7 ms @0.995 | **2.7 ms @1.000** | |
| kNN, HNSW efs=100 / efs=200 (default) | | — | 3.0 ms @1.000 | 3.6 / 5.2 ms @1.000 | |
| kNN, exact (`use_knn := true`) | 1.000 | 31 ms (sqlite-vec) | 940 ms | 18 ms | 0.6× |
| List review thread (property scan of 1k) | 22 | 0.011 ms | 0.40 ms | 1.07 ms | 97× |
| Floor: `RETURN 1` / PK lookup (prepared) / PK lookup (unprepared) | | ~0.005 ms | 0.038 / 0.091 ms | 0.13 / 0.21 / 0.34 ms | |

Notes:

- **Recall** is over the same 20 perturbed queries against an exact Rust scan.
- **Per-query floor.** Each query fans out over 12 worker threads. With `max_num_threads=1` the floor drops to 0.026 ms, short queries get faster (depth ≤ 3 1.4 ms, file 7.3 ms, BM25 2.3–2.7 ms, HNSW efs=40 1.9 ms), and scans get slower (top-50 42 ms, core callers 99 ms). Opening and querying in the same process with `max_num_threads=1` is a reasonable default for a CLI.
- **The "one file" query is slow because of the plan.** `MATCH (f:File {id:$id})-[:CONTAINS]->(s)-[:CALLS]->(t) RETURN t.id` plans a semi-mask scan of all of `Symbol` plus a hash join to fetch `t.id`, about 2 ms per statement. Point lookups (`QUERY_PRIMARY_KEY_LOOKUP`) cost about 0.2 ms.
- **The stale-summary query has no equi-join.** Equality on a non-key property is planned as a cross product plus a filter. Fetching all rows and diffing in Rust is the idiom, and it beats SQLite's `json_each`.
- **Variable-length paths cap at 30 hops by default.** `* SHORTEST` with a large bound is the cycle-safe reachability form. Walk semantics with `DISTINCT` only suits small bounds.
- **A 64 MB pool hurts only the scan-heavy vector queries:** exact kNN goes from 18 to 92 ms, and HNSW efs=200 from 5.2 to 10.8 ms. Graph queries are unchanged.

### Incremental update (server steady state)

This test replaces one file (`f1234`) in one transaction:

- delete its 10 symbols (and their CONTAINS and CALLS edges);
- re-insert the symbols with their CONTAINS edges and 61 CALLS edges (28 out, 33 in);
- update the file's hash and summary, keeping the FTS index current;
- update its embedding, keeping the vector index current.

Each run is checked afterwards: 36 in-calls (all engines), and the new summary text is found by FTS. The SQLite and SurrealDB `update` subcommands were added for this note.

| | SQLite | SurrealDB | LadybugDB |
|---|---|---|---|
| Replace one file, 1 txn, durable, median | **6.7 ms** | 27.0 ms | 29.0 ms |
| Single-row insert, autocommit | 3.3 ms | 3.2 ms | 4.2 ms (`CREATE`); 4.1 ms (`SET` by PK) |

LadybugDB, by step (mean over 50 runs):

| Step | Time |
|---|---|
| `DETACH DELETE` 10 symbols | 8.9 ms |
| Create 10 symbols + CONTAINS | 1.3 ms |
| Create 28 out-calls | 1.6 ms |
| Create 33 in-calls | 1.6 ms |
| Set hash + summary (FTS index update) | 4.4 ms |
| Set embedding (HNSW insert) | 18.7 ms |
| Commit | 4.7 ms |

The edge statements use `UNWIND $e AS e MATCH (a:Symbol) WHERE a.id = e.a WITH a, e MATCH (b:Symbol) WHERE b.id = e.b CREATE (a)-[:CALLS]->(b)`, which plans two primary-key lookups. The comma form `MATCH (a {id:e.a}), (b {id:e.b})` costs 190–230 ms per statement (a cross product).

Caveats:

- SQLite's row includes no ANN maintenance, since vec0 is an exact table.
- HNSW updates leave dead graph edges behind. The vector extension's README recommends periodically dropping and recreating the index on write-heavy tables, which costs ~13 s at 20k vectors.
- `COPY … FROM (UNWIND $rows …)` also works on a non-empty table inside an explicit transaction. It is an alternative for larger batches.

### Two processes at once

Each process opens its own `Database`.

| Pair | Outcome |
|---|---|
| writer + writer | **Second fails at open:** `Could not set lock on file … db.lbug (Resource temporarily unavailable)` |
| writer + reader (read-write open) | The reader fails the same way. |
| writer + reader opened `read_only` (either order) | Both run. The reader sees the database as of its own open time, including WAL commits up to then. Later commits are invisible until it reopens. Behaviour across a concurrent checkpoint was not tested. |
| read_only + read_only | Both run. |
| read-write + read-write, read-only workload | The second fails at open. |

So the model is the same as SurrealDB's: **one read-write process**, which fits `ai serve`. Other processes can only take `read_only` snapshots, and these go stale. `enable_multi_writes` exists in `SystemConfig` and governs concurrent write transactions within one process; it was not measured.

## Build and packaging

- **The prebuilt archive needs a matching compiler.** The crate downloads `liblbug-static-linux-x86_64-compat.tar.gz` (30 MB, built with GCC 13.3, `LBUG_VERSION` selects the release; the default is "latest"). It compiles its `cxx` shim with the host compiler. With clang 21 / GCC 15 headers (this devenv's `CC=clang`), the binary segfaults in `Database::Database` inside `std::format`, because `std::format` is not ABI-stable across libstdc++ 13→15. Setting `CC`/`CXX` to nixpkgs `gcc13` fixes it. A nixpkgs package should not use this path anyway: it downloads during the build.
- **Extensions with the prebuilt archive.** No extensions are linked in. `INSTALL fts` fetches `https://extension.ladybugdb.com/v0.21.0/linux_amd64/fts/libfts.lbug_extension` into `~/.lbdb/extension/…`; `LOAD fts` dlopens it, so the binary needs `-rdynamic`. Every process must `LOAD` (+11 ms open). On this host the built-in HTTP client (cpp-httplib/OpenSSL) hung after connecting, while curl fetched the same URLs. The bench pre-places the curl-downloaded files. This is a runtime network dependency, which suits neither an offline nixpkgs build nor an air-gapped user.
- **Source build with static fts+vector works, with two workarounds:**
  1. The crate passes no CMake flags. `EXTENSION_STATIC_LINK_LIST=fts;vector` was set by prepending it to `extension/extension_config.cmake` in a copy of the tree, which was then passed via `LBUG_SOURCE_DIR`. The crates.io tarball has no `extension/` dir; the source is `git archive v0.21.2` plus the extensions submodule at its pin.
  2. `liblbug.a` then references `FtsExtension::load`/`VectorExtension::load`, but the crate links only `lbug`. The first clean build failed at the final link. The binary's `build.rs` links `extension/{fts,vector}/build/lib*_static.lbug_extension` (whole-archive) and `libsnowball.a`. The fts stemmer is redirected into `extension/fts/build` by a one-line CMake patch, so its path is known before lbug's build script has run.

  After that, both extensions are loaded at `Database::new` with no `INSTALL`/`LOAD` and no network. The extension CMake writes its archives into the source tree (`extension/*/build`), so a Nix build must copy the source to a writable directory.
- **The Nix sandbox build works.** `nix-sandbox/default.nix` is a `buildRustPackage` with `cargoLock`, a minimal crate on `lbug = "=0.21.2"`, `LBUG_BUILD_FROM_SOURCE=1`, and `LBUG_SOURCE_DIR` pointing at the patched tree copied into `$NIX_BUILD_TOP`. Inputs are cmake, python3, pkg-config and openssl. With `sandbox = true` and local-only builders, it builds in 14 m 12 s, with no downloads: the `curl` fallback in `build.rs` is never reached with `LBUG_BUILD_FROM_SOURCE`, and CMake has no FetchContent unless `ENABLE_BACKTRACES`. The result is a 29.5 MB unstripped binary. Run under `unshare -rn` (no network) with an empty `HOME`, it reports `FTS`/`VECTOR` as `STATIC LINK` and answers BM25 and HNSW queries. Two fixes were needed:
  - **nixpkgs GCC 16 rejects the bundled thrift** (`TTransport.h`: `uint32_t` without `<cstdint>`). `CXXFLAGS="-include cstdint"` works around it; clang 21 accepts it. This is an upstream bug.
  - The extension link args above.
- **Build cost.** The C++ engine plus two extensions dominate: 14 min wall on 12 threads, or ~150 CPU-min. That is about 2× SurrealDB's CPU time and an order of magnitude more than the prebuilt route. Toolchain: cmake, python3 (single-file header), openssl (linked: the extension installer uses HTTPS even when unused), libstdc++, libatomic.

## On-disk format stability

From `src/include/storage/storage_version_info.h`:

| Releases | Storage version | Upgrade |
|---|---|---|
| 0.12.0–0.16.1 | 40 | Before 0.17 the version had to match exactly, so any change needed export/import. |
| 0.17.x | 41 | From 0.17.0 (2026-05), `canReadStorageVersion` accepts all of 40… current. |
| 0.18.x | 42 | |
| 0.19.x | 43 | |
| 0.20.0–0.21.2 | 47 | |
| 0.22.0 (HEAD, unreleased) | 49 | |

- Newer binaries open older files and rewrite the header at the next checkpoint. The upgrade is forward only: an older binary refuses a newer file ("Trying to read a database file with a different version").
- Index formats (FTS/HNSW) live in extension catalog entries and are not separately versioned in that table.
- The extension ABI is pinned to `LBUG_EXTENSION_VERSION` (0.21.0 for all of 0.21.x), so a dynamically loaded extension must match the engine minor version.
- A dropped compatibility path, if one ever happens, falls back to export/import (`EXPORT DATABASE`/`IMPORT DATABASE`).

## What was not measured

- LadybugDB's `enable_multi_writes`.
- A cold page cache.
- Scaling beyond 50k symbols.
- Behaviour of a `read_only` reader across a writer's checkpoint.
- The prebuilt `perf` variant of the archive.
- An HNSW rebuild after many updates.
- Building without the `arrow` feature. That would drop ~60 crates; bulk load would then go through files or `UNWIND`.

## Rerun

```
cd /hatimthayyil/tmp/db-bench
./prepare-ladybug.sh          # v0.21.2 source + extensions pin, static-link patch; prebuilt fts/vector into ~/.lbdb
./build-times-ladybug.sh      # clean builds: target-clean-ladybug-{prebuilt,source}; wall/CPU/size
./run-ladybug.sh              # load, queries (+1 thread, 64 MB pool), small-pool loads, update, open, 2 processes
./run.sh                      # SQLite/SurrealDB as before; then `bench-{sqlite,surreal} update data/{sqlite,surreal}`
(cd nix-sandbox && nix build --impure -f default.nix --option builders '' --option build-dir $PWD/../nix-build)
rm -rf target target-clean-* data src-ladybug nix-build nix-sandbox/result ~/.lbdb
```

- `bench-ladybug` takes `load|query|update|open|writer|reader|sql DIR`.
- Environment variables: `BENCH_BP_MB` (buffer pool in MB), `BENCH_THREADS` (`max_num_threads`), `BENCH_READONLY`, `BENCH_LOAD_EXT` (`open`: also load extensions) and `BENCH_DEBUG` (open stage timings).
- `env.sh` puts nixpkgs cmake, python3, ninja and openssl on PATH and sets `LBUG_VERSION=0.21.2`.
- The store is ~100 MB. A full LadybugDB run takes ~3 min after the builds.
