# Prior art: rolled-up summaries of file trees

Author: Claude Code (Claude Opus 5.5)

Prior art for a new `ai` store that summarises a file tree with an LLM and rolls the summaries up along the directories. The plan under review:

1. Each file is summarised at 10:1 by bytes, with a minimum size, and no chunking.
2. Each directory's summary is at least 2:1 smaller than its children's summaries combined.
3. Sizes are in bytes, and the size accepted is well above the size asked for, so length never forces a retry.
4. Rebuilds are incremental.
5. Ignores apply on top of `.gitignore`.

## Verdict

The plan agrees with prior art on three points: parents are built from their children's summaries only, rebuilds are keyed by content hash, and ignores use gitignore syntax on top of `.gitignore`. It differs on three:
- **No chunking.** Every LLM tool surveyed splits large inputs. One study measured whole-file summaries missing 9 to 11% of functions.
- **2:1 per directory** is gentler than the 3.6:1 to 10:1 per level others report, so upper levels stay larger.
- **Size proportional to each file** has no precedent. Others use a fixed cap per level or no target.

Measuring in bytes rather than tokens is new as well. It doesn't depend on any tokenizer.

## The tools

| Tool | What it builds | LLM summaries |
|---|---|---|
| Aider repo map | ranked symbols and signatures per file | none: tree-sitter plus PageRank |
| RAPTOR | tree of summaries over 100-token chunks, clustered by embedding | yes |
| Recursive book summarisation (OpenAI, Wu et al. 2021) | fixed tree over a book's chunks | yes |
| RepoAgent | docs per class and function, along the directory/file tree | yes, per object |
| Hierarchical repository summarisation (TCS, ICSE LLM4Code 2025) | segment, then file, then package, then repo | yes |
| CodeWiki (FSoft, 2025) | module tree, parents written from child docs | yes, agentic |
| DeepWiki (Cognition) | wiki pages planned by clustering | yes |
| Google Code Wiki | wiki regenerated after each change | yes (Gemini) |
| Cursor indexing | embeddings of chunks, synced by Merkle tree | no (embeddings) |
| Repomix | the whole repo packed into one file | no |
| LlamaIndex `tree_summarize` | packs chunks to the context window, recurses until one is left | yes |

## Per point

### 1. Files at 10:1, minimum size, no chunking

- **Ratio:** Wu et al. aim for "a text compression rate of 5-10x at each step". Leaf inputs are about 600 tokens, with hard caps of 128 tokens at the leaves and 192 at height 1. The 10:1 plan sits at the strong end of that range.
- **Proportional size:** no surveyed tool scales a file's summary with the file's size. Wu et al. cap each height at a fixed size. CodeWiki and the TCS paper set no numeric target, and TCS only asks that "the summary should be concise". Aider has one budget for the whole map (`--map-tokens`, default 1k, doubled by `--map-multiplier-no-files 2` when no files are in the chat).
- **Minimum:** no tool has one, because none summarises tiny inputs. RAPTOR keeps its 100-token leaves as raw text in the tree. That supports passing files below the minimum up whole.
- **Chunking:** every LLM tool splits large inputs.
  - Wu et al. use about 600-token chunks.
  - TCS uses AST segments. It found whole-file summaries less complete: GPT-4 missed about 9% of functions and 11% of variables, and Llama-3.2 missed 11% and 24%.
  - CodeWiki uses leaf modules of up to 32,768 tokens and delegates bigger modules to subagents, at most 3 levels deep.

  Without chunking, a size cap is the usual guard: Repomix skips files above `input.maxFileSize` (default 50 MB).

### 2. Directories at least 2:1

- **Others compress harder per level.** RAPTOR's summaries are 0.28 of their children combined (131 tokens over 6.7 children of 85.6 each), "a 72% compression rate". Wu et al. use 5-10x per step.
- **At the top, the ratio follows the fan-out.** Wu et al. note "the compression rate at top of the tree is typically lower, depending on the number of children of the root". With a fixed ratio and directories of any width, upper summaries grow with the fan-out. That matches the plan's "at least half".
- **Input is the children only.** TCS builds package summaries from "only the file summary", omitting segment detail. CodeWiki gives parents "child module documentation, module tree structure, dependency information". The plan agrees.
- **Errors don't climb.** RAPTOR hand-checked 150 nodes: 4% held a hallucination, usually minor, and these "did not propagate to higher layers".

### 3. Bytes, wide margin, no retries

- **Units:** everyone else counts tokens: Aider, RAPTOR, Wu et al., CodeWiki, and Repomix with `tokenCount.encoding` `o200k_base`. Bytes avoid tying the store to one model's tokenizer.
- **Margin:** Wu et al. set hard limits and "do not frequently hit these length limits". When one was exceeded, they truncated the summary rather than retrying. Aider fits its map with a binary search that stops within 15% (`ok_err = 0.15`). The plan takes the margin but fails a summary that overshoots, rather than truncating it.
- **Why the margin is needed:** LIFEBench (NeurIPS 2025, 26 models, 16 to 8,192 words) found that most models follow short length targets reasonably but "deteriorate sharply beyond a certain threshold", and often stop short.

### 4. Incremental rebuilds

- **Cursor** keeps "a cryptographic hash of every file, along with hashes of each folder" built from its children. It walks only the branches whose hashes differ, and it caches embeddings by chunk content. Clones in one organisation share 92% of their content, so indexes are reused across users. This is the plan's content-hash key.
- **RepoAgent** runs from a Git pre-commit hook and "only updates the documentation of affected objects": changed code, lost callers or new callers.
- **Aider** caches tags by path and file mtime (`.aider.tags.cache.v3`), which is weaker than a content hash.
- **Google Code Wiki** "scans the full codebase and regenerates the documentation after each change". It doesn't say whether the regeneration is incremental.
- **DeepWiki** regenerates by hand, or when a repository is connected. RAPTOR and CodeWiki describe full builds only.

### 5. Ignores on top of `.gitignore`

- **Cursor:** `.cursorignore` in gitignore syntax, on top of `.gitignore` and a built-in default list (lock files, `.env*`, `node_modules/`, binaries). `!` re-includes a file. Nested ignore files are opt-in.
- **Aider:** `.aiderignore` at the git root (`--aiderignore`). Files listed in `.gitignore` are out of scope by default (`--add-gitignore-files` false).
- **Repomix** has both forms: `.repomixignore` files and `ignore.customPatterns` in its config. It also reads `.gitignore`, `.git/info/exclude` and `.ignore`, plus default patterns. Priority runs from custom patterns, to ignore files (deeper ones win), to the defaults. Binary files are listed by path without their content.
- **DeepWiki** has no ignore setting. `.devin/wiki.json` steers the wiki with `repo_notes` (at most 10,000 characters each) and `pages` (at most 30, or 80 for enterprise), and "any folder without a page won't appear".

The plan's `ai.toml` list in gitignore syntax matches Repomix's `ignore.customPatterns`. A built-in default list, for lock files and binaries, is common practice and not in the plan.

## Not in the plan, seen elsewhere

- **Steering notes:** DeepWiki's `repo_notes`, and TCS's business context in the prompt, which improved relevance.
- **References between files:** Aider ranks by references, and RepoAgent and CodeWiki add caller and callee context. The plan rolls up along directories only. See below.

## Cross-cutting context

A summary tree rolls up along directories, so it misses what lives in other or lateral files: the caller three folders away, the config that sets a value, the doc that must change with the code. How established tools give an agent that context, for `ai summary` (the future name of `ai files`).

### The tools

| Tool | Finds related files by | Stores | Builds | Stays fresh by |
|---|---|---|---|---|
| Aider repo map | tree-sitter definitions and references; PageRank personalised to the chat's files and mentioned names | tags per file | every request, map cached | file mtime |
| RepoAgent | Jedi references: callers and callees per object | Markdown per object | Git pre-commit hook | regenerates an object when its code changes or its referrers change |
| CodeWiki | tree-sitter graph (calls, inheritance, attribute access, imports), split into feature modules | Markdown, module tree, registry of documented components | full build | not described |
| DeepWiki | "cluster-based planning", or pages listed in `.devin/wiki.json` | wiki pages | on connect, or by hand | regeneration by hand |
| Cursor | embeddings trained on agent traces, plus grep | chunk embeddings | background | Merkle tree of hashes |
| Sourcegraph Cody | BM25-style keyword search plus open files; embeddings dropped | server search index | server | server indexing |
| Augment | custom retrieval models, one index per user and branch | embeddings, "easily" 10 GB for large codebases | real time | "within a few seconds of any change" |
| Claude Code | agentic search: grep, glob, read | nothing | never | always current |
| Agentless | repo tree to the LLM, plus embeddings of 512-token chunks; then skeletons (signatures only) | nothing | per issue | not needed |
| LocAgent | graph of directories, files, classes, functions; edges contain, import, invoke, inherit; BM25 over entity ids; agent traverses | graph and sparse index | "a few seconds per codebase" | rebuild |
| CodeGraph 1.6.2 | tree-sitter graph (calls, imports, extends, implements, dynamic dispatch), FTS5 | `.codegraph/codegraph.db` (SQLite) | `codegraph init` | file watcher, 2 s debounce |
| Serena | language servers (40+ languages) or JetBrains: find symbol, referencing symbols, implementations | the language server's state | on demand | the language server |
| SCIP | per-language indexers (rust-analyzer, scip-python, …) emit symbol occurrences | Protobuf index | CI or auto-indexing | per upload |
| ROSE (co-change) | files changed together in version history | association rules | from history | every commit |

### Cost

- **Graphs are cheap.** No LLM. LocAgent builds in seconds. CodeGraph indexes the Swift compiler (about 27k files) in about 100 s, re-syncs a one-file edit in about 4 s, and spends about 0.3 s per save on a 4,400-file project.
- **LLM prose is not.** RepoAgent spent 4.1M prompt and 0.9M completion tokens (GPT-4) on AutoGPT. DeepWiki costs 5–10 ACUs at medium effort, 20–40 at high.
- **Narrow views pay.** Agentless's skeletons held the code to change 58.33% of the time against 53.67% for whole files, at $0.02 against $0.15.

### What the evidence says

- **Context nobody asked for hurts.** Context files, LLM-generated or developer-written, "do not generally improve task success rates" and raise inference cost "by over 20% on average"; repository overviews are "not helpful" (Evaluating AGENTS.md, ICLR 2026).
- **Structure beats similarity for localisation.** File-level Acc@5 on SWE-bench Lite: BM25 61.68, CodeRankEmbed 84.67, Agentless 79.56, LocAgent 94.16 (Claude 3.5). Without its entity search, LocAgent drops to 68.98.
- **Retrieval beside grep helps.** Cursor reports 12.5% higher answer accuracy (6.5–23.5% by model) with semantic search than grep alone, and best results with both.
- **Agentic search alone is a strong baseline.** Claude Code dropped its code index; agentic search "outperformed everything else by a lot", judged mostly by "vibes", and an index "drifts out of sync", must "live somewhere" and leaks code to a third party.
- **A graph tool cuts exploration.** CodeGraph reports, self-measured, 88% fewer tool calls, 62% fewer tokens and 44% lower cost for Claude Code across seven repos, with about 80% more retrieval context left in the window.
- **Code graphs miss lateral prose.** In multi-file localisation, "documentation evolution is a latent dependency that no approach resolves". History catches some of it: ROSE predicted 26% of further files to change after a first change, with a correct location in its top three 64% of the time.

### Usual practice

1. **No tool relies on a directory tree for cross-cutting context.** Related files come from a symbol graph (Aider, RepoAgent, CodeWiki, LocAgent, CodeGraph, Serena, SCIP), from retrieval at question time (Cursor, Cody, Augment), or from the agent searching (Claude Code).
2. **Agent-facing tools hand over structure and verbatim code;** LLM prose is for human-facing docs (RepoAgent, CodeWiki, DeepWiki, Code Wiki).
3. **Graphs are built by parsing and kept fresh per file change;** LLM summaries are built rarely and incrementally.
4. **Where prose uses neighbours,** it limits the cascade. RepoAgent puts callers and callees in the prompt, builds bottom-up, drops cycles, and does not regenerate an object when what it references changes. CodeWiki cross-references other modules instead of copying them.

### Recommendation for `ai summary`

- **Keep file summaries keyed by the file's content alone.** Neighbour summaries in the prompt would make each summary depend on its neighbours, so one edit would invalidate every dependent's summary, and builds would need a dependency order and cycle handling. RepoAgent shows the price, and still limits its cascade. The prompt already asks what a file depends on and what depends on it.
- **Record related files as links, built without an LLM, and show them in the view** (`related: …` under each summary), so the agent knows where to read next. In order of cost:
  1. paths the summary itself cites: free, already written;
  2. co-change from git history through gix: language-agnostic, and catches the lateral prose, config and tests a code graph misses;
  3. static imports and calls: read from a code graph when one is there, rather than building one.
- **Leave concepts to `ai note`.** A concept spanning five folders is a note that links its files, written when an agent learns it. Generated concept pages (DeepWiki, CodeWiki) serve humans browsing, and generated overviews did not help agents.
- **Don't build a code graph.** CodeGraph, language servers (Serena) and SCIP indexers already answer code-to-code questions at question time, fresh to the second. `ai summary` covers what they can't: purpose, prose, config, and decisions.
- **Keep summaries pulled, never pushed.** The agent reads them through view and zoom when it chooses. Measure any change on real tasks before adopting it, as the AGENTS.md study advises.

## Sources

- Aider, repository map: https://aider.chat/docs/repomap.html; options: https://aider.chat/docs/config/options.html; source: https://github.com/Aider-AI/aider/blob/main/aider/repomap.py
- RAPTOR (Sarthi et al. 2024), Appendix C Table 10 and Appendix E: https://arxiv.org/abs/2401.18059
- Wu et al. 2021, Recursively Summarizing Books with Human Feedback, Appendix A: https://arxiv.org/abs/2109.10862
- RepoAgent (Luo et al. 2024): https://arxiv.org/abs/2402.16667
- Dhulshette, Shah, Kulkarni 2025, Hierarchical Repository-Level Code Summarization for Business Applications Using Local LLMs: https://arxiv.org/abs/2501.07857
- CodeWiki (FSoft-AI4Code 2025), Appendix G: https://arxiv.org/abs/2510.24428
- DeepWiki docs: https://docs.devin.ai/work-with-devin/deepwiki
- Google Code Wiki: https://developers.googleblog.com/en/introducing-code-wiki-accelerating-your-code-understanding/
- Cursor, Securely indexing large codebases: https://cursor.com/blog/secure-codebase-indexing; ignore files: https://cursor.com/docs/context/ignore-files
- Repomix configuration: https://repomix.com/guide/configuration
- LlamaIndex response synthesizers: https://developers.llamaindex.ai/python/framework/module_guides/querying/response_synthesizers/
- LIFEBench (Zhang et al., NeurIPS 2025): https://arxiv.org/abs/2505.16234
- RepoAgent, Sections 3 and 4, Table 3: https://arxiv.org/html/2402.16667
- CodeWiki, Section 3: https://arxiv.org/html/2510.24428
- Cursor, Improving agent with semantic search: https://cursor.com/en-US/blog/semsearch
- Sourcegraph, How Cody understands your codebase: https://sourcegraph.com/blog/how-cody-understands-your-codebase
- Augment, A real-time index for your codebase: https://www.augmentcode.com/blog/a-real-time-index-for-your-codebase-secure-personal-scalable
- Boris Cherny on Claude Code's search, as reported by OfficeChai (2025-05-27): https://officechai.com/ai/claude-researcher-explains-how-agentic-search-performed-better-than-rag-for-code-generation/
- Agentless (Xia et al. 2024), Sections 3.1 and 4, Tables 1 and 2: https://arxiv.org/html/2407.01489
- LocAgent (Chen et al., ACL 2025), Sections 3 and 5, Tables 4 and 9: https://arxiv.org/html/2503.09089v2
- CodeGraph: https://github.com/colbymchenry/codegraph; version from `codegraph version` (1.6.2)
- Serena: https://github.com/oraios/serena
- SCIP: https://github.com/sourcegraph/scip; design: https://github.com/sourcegraph/scip/blob/main/docs/DESIGN.md; indexers: https://sourcegraph.com/docs/code-search/code-navigation/precise_code_navigation
- Evaluating AGENTS.md: Are Repository-Level Context Files Helpful for Coding Agents? (ICLR 2026): https://arxiv.org/abs/2602.11988
- Exploration Structure in LLM Agents for Multi-File Change Localization (2026): https://arxiv.org/abs/2606.11976
- Zimmermann et al., Mining Version Histories to Guide Software Changes (ICSE 2004): https://www.st.cs.uni-saarland.de/papers/icse2004/
