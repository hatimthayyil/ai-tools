# Storage landscape for `ai`

Survey of embedded stores and prior art for the storage foundation of planned features: code knowledge graphs, per-repo ontologies, per-file summaries rolled up to directories and repos, multi-agent `ai review`. Date: 2026-10-12. Builds on `20261003T104835--memory-db.md` and `20261003T105008--platform-db.md`; their conclusions stand unless stated below. No benchmarks here (separate agent). No code changed.

Constraints carried over: one local Rust binary, CC0, packaged in nixpkgs; many short-lived processes, parallel agent sessions writing concurrently; no daemon preferred; agents must query in complex ways.

## Summary

- **Primary store: SQLite** (rusqlite, bundled, WAL, FTS5; `sqlite-vec` when vectors are needed). It is the only embedded engine with mature cross-process writes without a daemon. Graphs go in node/edge tables with recursive CTEs; SQL is the language LLMs write best. This is what CodeGraph, codebase-memory-mcp, Codex, Aider and tuicr-class tools converged on.
- **Shared, mergeable data stays in git** (refs/notes, append-only ops), with SQLite as a rebuildable index. Gerrit (NoteDb), git-bug, git-appraise and Letta MemFS all do this for review/memory state. Summaries key naturally on git blob/tree oids.
- **Watch DoltLite** (SQLite fork with Dolt branches/merge, now has a Rust crate) for versioned, mergeable tables. **Keep LadybugDB** (Kuzu fork, Cypher) as the graph-native option if SQL graph queries prove too clumsy, only as a derived read-mostly index.
- **SurrealDB embedded stays out.** New since 2026-10-03: nixpkgs marks `bsl11` as `free = false`, so linking SurrealDB makes `ai` unfree in nixpkgs (no Hydra builds, needs `allowUnfree`). Embedded is still single-process.
- Most graph DBs (Ladybug, Grafeo, Oxigraph, Cozo, IndraDB, HelixDB) take an exclusive writer lock per process. Each one needs a daemon for parallel agents. Beads hit this and now auto-spawns a per-scope Dolt server.

## Changed since 2026-10-03

| Item | Was (Oct 3 notes) | Now |
|---|---|---|
| SurrealDB | 3.3.0-nightly | 3.3.0 stable (notes 2026-09-24; GH release 09-28); crate 3.3.3 (10-10). Automatic one-way data migrations on first start ("take an export first if you may need to roll back"). Still BSL 1.1 (change date 2030-01-01). Still no embedded multi-process. 2.7.0 also cut 09-28. |
| nixpkgs + BSL | "not OSI open" | `lib/licenses/licenses.nix`: `bsl11 = { free = false; redistributable = true; }`. `ai` linking SurrealDB → unfree package. |
| DoltLite | beta 0.50, "needs custom -sys binding" | 0.50.17 (2026-10-09), **`cargo add doltlite`**, format v12 frozen for beta; multi-process: 1 durable writer (`SQLITE_BUSY` for others), live readers; remotes `file://` and HTTP (`doltlite-remotesrv`), **no git remote**. Apache-2.0. |
| Turso DB | beta v0.6 | v0.8.3 (2026-10-12), not 1.0; multi-process via `.tshm` sidecar **experimental**; FTS (tantivy) experimental; vectors exact only, ANN on roadmap. |
| LadybugDB | 0.19 | 0.21.2 (2026-10-01); `lbug` crate; Cognee default graph store, GitNexus store. |
| Beads / Gas City | embedded Dolt single-writer, server for multi-agent | beads 1.3.1 (09-30), 1.3.2-rc.2; Gas City v1.5: bd owns a resident **proxied `dolt sql-server` per scope** (`bd db-proxy-child`). The no-daemon path did not survive multi-agent use. |
| SQLite | — | **WAL-reset bug**: corruption possible when ≥2 connections in separate threads/processes write or checkpoint at the same instant; versions 3.7.0–3.51.2; fixed 3.51.3 (2026-03-13). rusqlite 0.40.2 → libsqlite3-sys 0.38.2 bundles 3.53.2 (fixed). Never link a system SQLite < 3.51.3. Codex asserts `SQLITE_VERSION_NUMBER >= 3_051_003`. Latest 3.54.0 (10-09). |
| DuckDB | one writer process | 1.5.6 (09-28); 2.0 announced for Sept, not shipped. Multi-process writes only via **Quack remote protocol** (beta since 1.5.2, client-server) or DuckLake + Postgres catalog. |
| Codex CLI | JSONL + `state.db` | rust-v0.162.1; `state_5.sqlite` (61 migrations, sqlx), memory pipeline moved to its **own SQLite DB** because it is rebuildable (PR #24591, 2026-05-26). |

## Embedded stores

### Overview

Health = last release, 90-day commit authors (GitHub API, 2026-10-12). nixpkgs column: whether the licence keeps `ai` free in nixpkgs.

| Store | Last release | 90-day activity / bus factor | Licence → nixpkgs | Multi-process, no daemon | Query language | Graph / FTS / vector | Footprint |
|---|---|---|---|---|---|---|---|
| **SQLite** (rusqlite 0.40.2) | 3.54.0 (10-09); rusqlite 08-08 | consortium + rusqlite team; very low risk | public domain / MIT → free | **yes**: WAL, N readers + 1 writer, `busy_timeout` | SQL | CTEs / FTS5 / `sqlite-vec` | +2 MB, 45 s build (measured) |
| sqlite-vec | 0.1.9 (03-31); last push 05-18 | asg017 alone, Mozilla-sponsored; quiet 5 months | MIT/Apache → free | as SQLite | SQL (`vec0`) | — / — / brute force; DiskANN/IVF files in repo, undocumented | C ext, small |
| GraphQLite | 0.9.4 (10-12) | dylanbstorey alone | MIT → free | as SQLite (plain tables) | **Cypher** in SQLite (openCypher TCK 98.5%) | PageRank, Louvain, Dijkstra / — / — | C ext, Rust crate |
| Turso DB | 0.8.3 (10-12) | penberg, jussisaurio, LeMikaelF; company | MIT → free | experimental (`.tshm`); `BEGIN CONCURRENT` MVCC | SQL (SQLite) | CTEs / tantivy FTS (exp.) / exact vectors | pure Rust, moderate |
| **DoltLite** | 0.50.17 (10-09) | tim-ning, timsehn (DoltHub); company | Apache-2.0 → free | yes: 1 writer + live readers | SQL + `dolt_*` procedures | as SQLite + branches/merge/diff | C fork of SQLite; small |
| Dolt | 2.4.2 (10-06) | DoltHub | Apache-2.0 → free | **no**: embedded single-writer; server for many | SQL (MySQL) | — / limited / yes | Go, ~100 MB, subprocess |
| DuckDB (+DuckPGQ, VSS, FTS) | 1.5.6 (09-28); duckdb-rs 09-30 | DuckDB Labs; strong. DuckPGQ: 1 author, "research project" | MIT → free | **no**: 1 RW process or N read-only; Quack server (beta) | SQL; SQL/PGQ via DuckPGQ | PGQ / FTS (rebuild on change) / HNSW (persistence experimental) | C++, ~30 MB, slow build |
| SurrealDB 3.3 | 3.3.0 (09-24/28), crate 3.3.3 | dev in private repo; public mirror | **BSL 1.1 → unfree** | **no** (exclusive lock; docs: "single process") | SurrealQL (+GQL default-on in 3.3) | `RELATE` / BM25 / HNSW, DiskANN | 105 MB, 8.6 min build (measured) |
| **LadybugDB** (Kuzu fork) | 0.21.2 (10-01) | adsharma 325/386 commits; bus factor ~1 | MIT → free (in nixpkgs as `ladybugdb`) | **no**: one READ_WRITE `Database` per file, or N READ_ONLY; mixing RW + RO processes "not safe"; docs recommend an API server | Cypher | native / FTS / vector | C++ via cmake; heavy |
| Kuzu | 0.11.3 final (2025-10-10) | archived (Apple) | MIT | as Ladybug | Cypher | | dead |
| RyuGraph (Kuzu fork) | 25.9.2 (2025-12-06) | last push 2026-01-20 | MIT | as Ladybug | Cypher | | stale |
| Grafeo | 0.5.44 (10-04) | StevenBtw 283/300; bus factor 1 | Apache-2.0 → free | **no**: `try_lock_exclusive` on the file | GQL, Cypher, Gremlin, SPARQL, SQL/PGQ, GraphQL | LPG+RDF / BM25 / HNSW | pure Rust; young |
| HelixDB v3 | 3.5.1 (10-08) | xav-db, matthewsanetra; startup | Apache-2.0 (was AGPL) → free | no: SlateDB (object-storage LSM, single writer), git-pinned fork | SDK DSL + Cypher reads | graph / BM25 / vector | Rust; cloud-first |
| Oxigraph | 0.5.11 (09-02) | Tpt 87/108; bus factor 1 | MIT/Apache → free | **no**: one RW `Store`; `open_read_only` beside a writer is "undefined behavior" | **SPARQL 1.1** (1.2 preliminary) | RDF / — / — | RocksDB C++; heavy |
| CozoDB | 0.7.6 (2023-12-11) | last push 2024-12 | MPL-2.0 | backend-dependent (SQLite backend) | Datalog (CozoScript) | yes / yes / HNSW | **unmaintained** |
| IndraDB | 5.0.0 (2025-08-16) | inactive since | MPL-2.0 | no (RocksDB/memory) | Rust API, gRPC | graph only | stale |
| TypeDB 3 | 3.13.6 (09-22) | active company | MPL-2.0 | **server only** | TypeQL | typed hypergraph, strong schema | out |
| TerminusDB | 12.0.8 (10-12) | Gettier team | Apache-2.0 | **server only** (Prolog + Rust store) | WOQL, GraphQL | git-like commits, push/pull, JSON-LD schema | out |
| LanceDB / Lance | 0.40.0 / 13.0.0 (10-07) | company; strong | Apache-2.0 → free | yes-ish: optimistic commits, limited retries under contention | SQL-ish via DataFusion | — / FTS / **vector core** | Arrow/DataFusion; heavy |
| FalkorDB / FalkorDBLite | 6.0.2 / 0.10.0 | company | **SSPL → unfree** | Lite spawns a Redis subprocess | Cypher | | out on licence |
| tantivy | 0.26.1 (05-10) | quickwit; healthy | MIT | 1 writer (lockfile) + N readers | query DSL | FTS only | pure Rust |
| redb / fjall | 4.3.0 / 3.1.12 | single maintainers each, active | MIT/Apache | **no** (exclusive lock) | KV | — | tiny |

Sources: GitHub API (`repos/*`, `releases/latest`, `commits?since=2026-07-12`), crates.io API, per-project docs linked below.

### Notes per family

**SQLite.** Graph = `node(id, kind, …)`, `edge(src, dst, kind, …)` with indexes on `(src, kind)` and `(dst, kind)`; traversal by recursive CTE. CodeGraph runs on exactly this (schema below) and ships to ~74k-star scale. Cypher when wanted: GraphQLite stores ordinary tables, so WAL concurrency is preserved, but it is a single-maintainer C extension. FTS5 for tokens/BM25, `trigram` for substrings; regex stays a scan. Vectors: `sqlite-vec` is pre-v1, brute force documented, sole maintainer, quiet since May: fine at code-repo scale (≤1e5–1e6 vectors), a risk beyond.

**Turso DB.** The SQLite-compatible path to multi-writer MVCC and built-in FTS/vectors. Cross-process access is experimental and the project is pre-1.0 ("keep independent backups"). Re-check at 1.0.

**DoltLite.** The only embedded store that gives versioned tables with branch/diff/merge and SQLite's process model. Fits review state and summary caches that must merge across machines or agents (branch per agent, merge on accept). Gaps: beta, format frozen only within beta, transport is file/HTTP, not a git remote (Dolt proper does `refs/dolt/data`; DoltLite does not, per README). The memory-db note asked for a Rust binding and a year of stability: the binding exists, the stability does not yet.

**DuckDB.** Best for analytics over many repos (scan Parquet/JSON, SQL/PGQ path queries). Disqualified as the shared write store: one writer process. Usable as a read-only analytical lens over SQLite files (`ATTACH … (TYPE sqlite)`).

**LadybugDB.** Strongest embedded graph engine (columnar, CSR adjacency, worst-case-optimal joins, FTS, vector, Cypher). Adopted by GitNexus and Cognee. Process model rules it out as the write store for parallel agents: one RW process, and RO readers are unsafe beside a writer. Viable only as a derived index built by one process and swapped atomically (write new file, rename), read RO by others. Bus factor ~1 after the Kuzu team left.

**SurrealDB.** Model fit is still excellent (see platform note). Blockers now: unfree in nixpkgs, single-process embedded, 105 MB, one-way format migrations on minor upgrades, private development. The benchmark agent covers performance; none of these blockers are performance.

**Oxigraph.** The right engine if the ontology must be RDF/OWL with SPARQL. RocksDB single-writer; one maintainer; SPARQL evaluation "not yet optimized". Can run in-memory (`Store::new()`) loaded from Turtle files on demand, which sidesteps the lock for read-only ontology queries.

**Grafeo / HelixDB.** Promising, young, single-maintainer or cloud-first. Grafeo speaks six query languages but locks exclusively. HelixDB v3 moved onto a forked SlateDB (object storage). Neither fits now.

## Query languages and LLM fluency

| Language | Training data volume | Evidence | Fit for agents |
|---|---|---|---|
| SQL (SQLite dialect) | very high | Spider ~86–87% exec accuracy (DAIL-SQL); SM3 benchmark: schema info helps SQL/Cypher/MQL a lot | best; agents already use `sqlite3` |
| Cypher | moderate–high | Text2Cypher 2026: frontier models ~87–88 on MindTheQuery, ~63–65 on Text2GQL-Bench | good for path patterns; dialect gaps (Kuzu/Ladybug needs typed schema, not Neo4j-compatible everywhere) |
| SPARQL | moderate | SM3: for SPARQL, MQL, partly Cypher, "LLMs are often unable to generate a correct query with only the schema" | weak unless prefixes and shapes are given |
| SurrealQL | low, and stale: 2.x→3.x renamed `SEARCH`→`FULLTEXT`, `MTREE`→`HNSW`, functions | no benchmark found | weakest; needs docs in context |
| Datalog (CozoScript) | very low | none | poor; and Cozo is unmaintained |
| GQL / SQL/PGQ | low (ISO 2023/2024) | none | close to Cypher; models transfer partly |

Takeaway: expose SQL (read-only connection, `PRAGMA query_only`) plus a schema description and a few named views (`callers_of`, `impact`, `subtree_summary`). That matches what code tools do: GitNexus offers raw Cypher but routes most use through 17 task tools; CodeGraph exposes `explore`, not SQL.

## Prior art

### Code intelligence

| Tool | Store | Where | Lessons |
|---|---|---|---|
| **CodeGraph** 1.6.2 (`colbymchenry/codegraph`, MIT, ~74k★) | SQLite WAL | `<repo>/.codegraph/codegraph.db`, self-gitignored (`*` except `.gitignore`); mentions `daemon.pid`, sockets | Schema (from `/hatimthayyil/code/statig/.codegraph`): `nodes(id, kind, name, qualified_name, file_path, language, start/end line/col, docstring, signature, visibility, flags, decorators JSON, return_type)`, `edges(source, target, kind, metadata JSON, line, col, provenance)` with unique `(source,target,kind,line,col)`, `files(path, content_hash, …)`, `unresolved_refs`, `nodes_fts` FTS5 via triggers, `project_metadata`, `schema_versions`. Edge kinds: calls, contains, references, imports, instantiates, extends, implements. Per-repo DB, content-hash invalidation, no summaries. |
| codebase-memory-mcp (DeusData, MIT, C, v0.11.0) | SQLite | `~/.cache/codebase-memory-mcp/` (secondary source) | Paper (arXiv 2603.27277): graph via MCP got 83% answer quality vs 92% for a file-exploring agent, at ~10× fewer tokens. Graph complements reading, it does not replace it. |
| GitNexus (PolyForm **Noncommercial**, ~48k★) | LadybugDB (native CLI; WASM in browser) | `<repo>/.gitnexus/` gitignored, or `~/.gitnexus/stores/<repo>/` shared by worktrees; `~/.gitnexus/registry.json` | Single `CodeRelation` edge table with `type` property; communities, processes; opt-in embeddings; hybrid BM25+vector+RRF; raw `cypher` tool exists but 17 task tools do most work; LLM `wiki` module docs. |
| code-graph-rag (MIT) | Memgraph + Qdrant via `cgr daemon up` | server | LLM writes Cypher. Daemon-based. |
| Sourcegraph SCIP (now `scip-code/scip`, Apache-2.0) | protobuf index per upload; Sourcegraph ingests to Postgres | server | Symbol strings as stable global ids; replaced LSIF graph format with a document-oriented one. Good id scheme to borrow for cross-repo symbols. |
| Aider repo map | `diskcache` (SQLite) tags cache | `<repo>/.aider.tags.cache.v4/` | tree-sitter tags → file graph → `networkx.pagerank` with chat-file personalization → fit to `--map-tokens` (1k default). Cache keyed by file mtime. Aider last release v0.86.0 (2025-08). |
| Serena (oraios) | files | `.serena/project.yml`, `.serena/memories/*.md`, `.serena/cache/<lang>/document_symbols_cache_*.pkl` | Memories as markdown in repo; LSP symbol cache as pickle. |

### Agent memory

| Tool | Store | Lessons |
|---|---|---|
| Graphiti / Zep (Apache-2.0, v0.30.2) | Neo4j, FalkorDB (+Lite), Neptune; **Kuzu deprecated, slated for removal** | bi-temporal edges, invalidate not delete; ontology = Pydantic entity/edge types. Embedded graph backends keep dying under it. |
| Cognee (Apache-2.0) | defaults: SQLite + LanceDB + **Ladybug** files under `databases/`; Postgres all-in-one option | docs warn the Kuzu/Ladybug store's file lock "is not suitable for concurrent access from multiple agents"; recommend Neo4j/FalkorDB. Ontologies: optional OWL/RDF file parsed with RDFLib, matched nodes flagged, class/property edges added. |
| Mem0 (Apache-2.0) | vector store + SQLite history; OSS `graph_store` (Neo4j, Memgraph, Neptune, Kuzu); Platform v3 replaced external graph with built-in entity linking | external graph DB was dropped from the hosted product: entity co-occurrence + multi-signal retrieval sufficed. |
| Letta | server moved to `letta-code`; **MemFS**: memory as markdown + YAML frontmatter in a local git repo (`~/.letta/agents/<id>/memory/`), synced with the API | text-in-git as memory truth; `system/` files always in prompt. |
| Codex CLI | JSONL rollouts (truth) + `state_5.sqlite` (sqlx) + separate logs DB + separate memory DB | split DBs by rebuildability; guard SQLite ≥ 3.51.3. |
| beads / Gas City | Dolt; now a resident proxied `dolt sql-server` per scope | versioned SQL is valuable enough to them to accept a daemon; JSONL kept as export. |

### Hierarchical code summaries

| Tool | Where summaries live | Update |
|---|---|---|
| RepoAgent (OpenBMB, Apache-2.0; last push 2024-12) | `markdown_docs/` in repo + JSON hierarchy in `.project_doc_record` | pre-commit hook regenerates changed files' docs |
| CodeWiki (FSoft-AI4Code, MIT, active) | `./docs/` in repo: `overview.md`, page per module, `module_tree.json`, `metadata.json` (model, commit); dependency graph in `temp/` | leaf modules by one agent each, parents synthesised from children; `--update` diffs saved graph, patches affected modules ($0.34 vs $21.48 full on svelte) |
| deepwiki-open (MIT) | local cache outside repo (`~/.adalflow`, per its source) | regenerate |
| GitNexus `wiki` | generated docs | LLM, opt-in |
| Directory `AGENTS.md`/`CLAUDE.md` | in repo, nested per directory | by hand or agents |

Pattern: when summaries are for humans they are committed markdown; when for agents they are a cache. Nobody keys them on git oids, though that is the natural key: file summary ↔ blob oid, directory summary ↔ tree oid (git trees are already a Merkle rollup; a changed file invalidates exactly its ancestor directories). Same shape as `ai memory`'s fp-keyed summaries. They can be stored as `(oid, model, prompt_version) → text` in SQLite and shared through a git ref or notes keyed by oid.

### Review tools

| Tool | Storage | Sharing / merge |
|---|---|---|
| **tuicr** (`agavra/tuicr`, MIT, v0.27.0 2026-09-23, Rust) | JSON per session in platform data dir `tuicr/reviews/sessions/<16-hex>.json` + `index.json` manifest (rebuildable from sessions), `active_sessions.json`; lockfile `.tuicr.lock` (10 s timeout, stale after 60 s) | not shared; exports markdown to clipboard/stdout for agents, or to forges (GitHub, GitLab, Gerrit…); `tuicr review` CLI to add/list comments for agents |
| git-appraise (Google, Apache-2.0; last push 2023-08) | git notes `refs/notes/devtools/{reviews,discuss,ci,analyses}`; one JSON object per line | `cat_sort_uniq` notes merge: line-set union; latest timestamp wins |
| git-bug (GPL-3.0, active) | entities as commit chains under `refs/<namespace>/<id>`; ops packs as JSON blobs; Lamport clocks per entity | push/pull refs; DAG merge, order by clock, tie-break pack id; local cache + index outside the tree |
| Gerrit NoteDb | change meta under `refs/changes/NN/N/meta`; inline comments as JSON in a NoteMap keyed by patchset sha; config in `refs/meta/config`; accounts in `All-Users` | git replication; secondary (Lucene) index rebuilt from refs. Moved off SQL ReviewDb for one source of truth, atomic snapshots with code, audit as commits, federation, offline review |

For `ai review`: concurrent reviewer/author/arbiter processes on one machine → SQLite WAL (atomic claims, threads, statuses). Durable/shareable record → append-only JSON ops in a git ref (git-appraise/git-bug style), merged by union under `(lamport|ts, origin)`, the same scheme as `ai memory`.

## Ontologies

What "per-repo ontology" means in practice for LLM agents, from most to least common:

1. **Glossary / ubiquitous language** in a markdown file the agent always reads: `CONTEXT.md` + `CONTEXT-MAP.md` (Matt Pocock's domain-modeling skill), `UBIQUITOUS_LANGUAGE.md`, `DOMAIN_GLOSSARY.md`, or a section of `AGENTS.md`/`CLAUDE.md`. Goal: stop agents inventing synonyms (User vs Customer). This repo already does it (`my/glossary.md`, `ai/glossary.md`).
2. **Typed entity/relation schema** that steers extraction: Graphiti Pydantic types, Cognee custom models, LinkML YAML (OntoGPT/SPIRES turns it into prompts and validates output). Kept as code/YAML in the repo.
3. **RDF/OWL (+SHACL)**: Cognee accepts OWL/Turtle via RDFLib; MOOSEDev (arXiv 2608.13662) uses two small OWL ontologies (code structure, architecture) with SHACL validation for decision/constraint memory, and reports near-complete recall on supersession/negation questions where vector memory surfaced 6–27%. Rare in practice for code repos; tooling is Python/JVM.
4. **DB schema as ontology**: TypeDB/TerminusDB put the ontology in the database. No coding-agent tool found doing this.

Practical reading: the ontology is a small, human-reviewed, versioned **text file in the repo** (glossary + typed kinds and relations, markdown or YAML), loaded into the DB as `kind`/`relation` vocabulary tables that constrain extracted facts. Typed, closed edge vocabularies matter more than OWL reasoning; the MOOSEDev result comes from typed records with supersession/status, which SQLite tables with `CHECK`/FK constraints also express. Keep RDF as an export format (Oxigraph in-memory) only if interop with external ontologies is wanted.

## Recommendation

Seriously consider:

1. **SQLite (rusqlite bundled ≥ 3.51.3, WAL, FTS5; `sqlite-vec` later) as the single local query store**, one file per repo in `$(git rev-parse --git-common-dir)/ai/` (shared by worktrees; not walked by rg). Schema per feature: CodeGraph-like `node/edge/file` for the code graph, `summary(oid, kind, model, prompt_v, text)` keyed by blob/tree oid, `term/relation` for the ontology vocabulary, `review/thread/comment/claim` for review. Agents query via read-only SQL plus named views. Rebuildable from git + repo files; Codex-style split into separate DB files where rebuild cost or churn differs. Meets every hard constraint today.
2. **Git refs for anything shared or mergeable** (review records, summaries worth sharing, memory as now), with the SQLite file as index. Proven by Gerrit, git-bug, git-appraise, Letta MemFS; no new dependency beyond the existing git-object store.
3. **DoltLite as the upgrade path** if versioned tables (per-agent branches of review/summary state, diff, merge) become central. SQLite dialect, same process model, Apache-2.0, Rust crate. Wait for post-beta format stability and a git-remote transport.

Not recommended now: SurrealDB (unfree in nixpkgs, single-process, format churn), LadybugDB/Grafeo/Oxigraph as the write store (exclusive locks → daemon). LadybugDB stays the fallback if graph analytics outgrow recursive CTEs: build it as a derived snapshot file, readers open it read-only after an atomic swap. Revisit Turso at 1.0 if multi-writer MVCC or built-in FTS/vector becomes worth a SQLite-compatible switch.

## Open questions

1. Is a resident per-repo process ever acceptable? It is the only way to use Ladybug/SurrealDB/Dolt with parallel writers (beads accepted it). If yes, the field reopens; if no, the choice is SQLite (+DoltLite later).
2. Where should summaries live: a local cache only, a shared git ref keyed by oid, or committed markdown (CodeWiki style) for humans too?
3. Are review records shared across machines/teammates (git ref) or local per session (tuicr style)?
4. Ontology format: markdown glossary only, or also a machine-checked typed schema (YAML/LinkML) that constrains extracted entities? Any need for RDF interop?
5. Cross-repo queries (one DB per repo vs a user-level DB with a repo column, or DuckDB attaching many SQLite files)?
6. Vector search: needed at all for code (CodeGraph and Aider do without), or are FTS + graph ranking enough?
7. Does the separate SurrealDB-vs-SQLite benchmark change any of this? Expected not, since the SurrealDB blockers here are licence and process model.

## Sources

Local:
- CodeGraph schema: `sqlite3 /hatimthayyil/code/statig/.codegraph/codegraph.db .schema` (codegraph 1.6.2).
- tuicr source: `/hatimthayyil/tmp/db-survey/tuicr/src/persistence/storage.rs` @ 92a3656 (2026-10-11).
- Grafeo lock: `/hatimthayyil/tmp/db-survey/grafeo/crates/grafeo-storage/src/file/manager.rs` (`try_lock_exclusive`).
- HelixDB storage deps: `/hatimthayyil/tmp/db-survey/helix-db/**/Cargo.toml` (slatedb fork, object_store).
- nixpkgs `lib/licenses/licenses.nix` (`bsl11`, `sspl` → `free = false`), `pkgs/by-name/{su/surrealdb,la/ladybugdb,ku/kuzu,du/duckdb,tu/turso,do/dolt}`, checkout 2026-10-11.
- libsqlite3-sys 0.38.2 crate tarball: bundled `SQLITE_VERSION "3.53.2"`.

Stores:
- SurrealDB 3.3 release notes: https://surrealdb.com/releases/3.3 ; licence: https://github.com/surrealdb/surrealdb/blob/main/LICENSE ; embedded single-process: https://surrealdb.com/docs/sdk/python/concepts/embedded-databases
- SQLite changes: https://www.sqlite.org/changes.html ; WAL-reset bug: https://www.sqlite.org/wal.html
- sqlite-vec: https://github.com/asg017/sqlite-vec ; GraphQLite: https://github.com/colliery-io/graphqlite
- Turso: https://github.com/tursodatabase/turso ; MVCC: https://turso.tech/blog/concurrent-writes-in-practice
- DoltLite: https://github.com/dolthub/doltlite ; https://dolthub.com/blog/2026-08-31-doltlite-beta ; Dolt: https://github.com/dolthub/dolt
- DuckDB concurrency: https://duckdb.org/docs/current/connect/concurrency.html ; 1.5.6: https://duckdb.org/2026/09/28/announcing-duckdb-156 ; DuckPGQ: https://github.com/cwida/duckpgq-extension
- LadybugDB: https://github.com/LadybugDB/ladybug ; concurrency: https://docs.ladybugdb.com/concurrency/ ; Kuzu archive: https://github.com/kuzudb/kuzu ; RyuGraph: https://github.com/predictable-labs/ryugraph
- Grafeo: https://github.com/GrafeoDB/grafeo ; HelixDB: https://github.com/HelixDB/helix-db
- Oxigraph: https://github.com/oxigraph/oxigraph ; `Store` docs: https://docs.rs/oxigraph/latest/oxigraph/store/struct.Store.html
- CozoDB: https://github.com/cozodb/cozo ; IndraDB: https://github.com/indradb/indradb ; TypeDB: https://github.com/typedb/typedb ; TerminusDB: https://github.com/terminusdb/terminusdb
- LanceDB FAQ (concurrent writes): https://docs.lancedb.com/faq/faq-oss
- FalkorDB licence: https://docs.falkordb.com/references/license

LLM query fluency:
- Text2Cypher (2024): https://arxiv.org/abs/2412.10064 ; grounded Text-to-Cypher (2026): https://arxiv.org/pdf/2606.14325
- SM3-Text-to-Query (NeurIPS 2024 D&B; SQL/MQL/Cypher/SPARQL on one dataset): https://arxiv.org/abs/2411.05521
- DAIL-SQL / Spider: https://arxiv.org/abs/2308.15363

Prior art:
- CodeGraph: https://github.com/colbymchenry/codegraph ; codebase-memory-mcp: https://github.com/DeusData/codebase-memory-mcp ; paper: https://arxiv.org/abs/2603.27277
- GitNexus: https://github.com/abhigyanpatwari/GitNexus ; code-graph-rag: https://github.com/vitali87/code-graph-rag ; SCIP: https://github.com/scip-code/scip
- Aider repo map: https://aider.chat/docs/repomap.html , https://github.com/Aider-AI/aider/blob/main/aider/repomap.py
- Serena: https://github.com/oraios/serena
- Graphiti: https://github.com/getzep/graphiti ; Cognee stores: https://docs.cognee.ai/guides/store-configurations ; Cognee ontologies: https://docs.cognee.ai/guides/ontology-support
- Mem0 graph memory (platform v3): https://docs.mem0.ai/platform/features/graph-memory
- Letta MemFS: https://docs.letta.com/letta-code/memfs ; https://github.com/letta-ai/letta-code
- Codex state migrations: https://github.com/openai/codex/tree/main/codex-rs/state/migrations ; memory DB split: openai/codex PR #24591
- Beads: https://beads.gascity.com/architecture/dolt ; Gas City v1.5.0 release notes: https://github.com/gastownhall/gascity/releases/tag/v1.5.0
- RepoAgent: https://github.com/OpenBMB/RepoAgent ; CodeWiki: https://github.com/FSoft-AI4Code/CodeWiki ; deepwiki-open: https://github.com/AsyncFuncAI/deepwiki-open
- tuicr: https://github.com/agavra/tuicr ; git-appraise: https://github.com/google/git-appraise ; git-bug data model: https://github.com/git-bug/git-bug/blob/master/doc/design/data-model.md ; Gerrit NoteDb: https://gerrit-review.googlesource.com/Documentation/note-db.html

Ontologies:
- Domain-modeling / ubiquitous-language skills: https://aihero.dev/skills-domain-modeling , https://www.agentpatterns.ai/instructions/ubiquitous-language-for-ai-plans/
- LinkML: https://arxiv.org/abs/2511.16935 ; OntoGPT: https://github.com/monarch-initiative/ontogpt
- MOOSEDev, ontology-grounded project memory: https://arxiv.org/abs/2608.13662
