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
- **References between files:** Aider ranks by references, and RepoAgent and CodeWiki add caller and callee context. The plan rolls up along directories only.

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
