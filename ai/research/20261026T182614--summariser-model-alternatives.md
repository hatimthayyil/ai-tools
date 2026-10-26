# Summariser models available by API

Author: Claude Code (Claude Opus 5.5)

The models `ai` could call by API to write its summaries in place of Sonnet 5.5: the public evidence on their faithfulness, their prices at our call sizes, and their sellers' data terms. Accuracy and correctness come first, then cost. Desk research on 2026-10-26, then our own evals of DeepSeek Flash 1017 and Opus 5.5 against Sonnet 5.5 on 2026-10-26 and 27 ([Our evals](#our-evals)). The note ranks and chooses nothing.

## What we measured

Today every summary is one `claude -p --model sonnet --effort medium` call with no tools, a fresh session per line, on the Claude subscription: every recorded call's init event says `apiKeySource: none`.

- **Memory** (`src/memory/prompts/compact.txt`, 2,416 characters): one note, or two neighbouring lines, into one line asked for in 50 words and accepted up to 512 characters (`NODE`, `src/memory/tree.rs`), up to 5 tries (`TRIES`, `src/memory/compact.rs`).
- **Threads** (`src/agent/prompts/compact.txt`, 3,895 characters): lines of up to 200 words (`MAX_WORDS`, `src/agent/rollup.rs`).

From the call records (`~/.ai/memory/calls`, `.ai/local/agents/threads/ai-chat/calls`), all `claude-sonnet-5-5`:

| Store | Results | Prompt tokens: avg (p50 / p90 / max) | Output tokens: avg (max) | Sessions retried |
|---|---|---|---|---|
| memory | 334 | 1,503 (1,695 / 1,933 / 2,325) | 161 (339) | 21 of 313 |
| thread | 807 | 3,274 (2,582 / 5,053 / 21,075) | 189 (879) | 27 of 810 |

- A full memory rebuild is up to 605 calls (`ai memory build --rebuild --dry-run`, 940 notes).
- The `ai-chat` thread has 807 recorded calls over 422 turns: 1.91 calls per turn, rebuilds included.
- The records price the calls at $0.0047 per memory call and $0.0082 per thread call (`total_cost_usd`, cache writes included). That is the API-equivalent price; the subscription pays.

### Our 2026-10-08 eval

`ai/evals/compactor/README.md` (2026-10-08, 83 notes) rebuilt the whole memory tree with each model from the same notes:
- Haiku 5.5 changed meaning when merging: "no API key, OAuth banned" became "no API-key/OAuth ban" and was carried to the top line; "Stage1 c2b5b31; old memory unreadable" became "Stage1 unreadable". No distortion was found in Sonnet 5.5's tree.
- Haiku took 456 s, Sonnet 116 s.
- Haiku 5.5 failed the eval and Sonnet 5.5 stayed the summariser.

The 2026-10-26 and 27 evals are in [Our evals](#our-evals).

## Models

How to read the table:
- **Vectara:** hallucination rate on Vectara's leaderboard (HHEM-2.3, updated 2026-10-22), lower means fewer; the answer rate is in brackets. All 111 board rows are here; rows that differ only in reasoning effort share a line, with the other effort in Notes.
- **FACTS G.:** FACTS Grounding v2 score (Kaggle board, read through BenchLM's mirror, 2026-10-13), higher means more grounded.
- **AA-Omni:** AA-Omniscience hallucination rate (Artificial Analysis, 2026-10-26), closed-book questions, lower means fewer; each model at the effort named in [AA-Omniscience](#aa-omniscience).
- A blank score means the board doesn't list the model.
- **$/M in / out:** list price per million tokens, standard tier, no cache or batch discount, fetched 2026-10-26.
- **Seller:** the model's maker where it sells an API: its pricing page for Anthropic, OpenAI, Google, DeepSeek and AI21, its endpoint in OpenRouter's API for Mistral, Alibaba, Z.AI, MiniMax, Moonshot, Cohere, Amazon, Inception and xAI. Otherwise the cheapest of DeepInfra, Together, Fireworks and Groq on OpenRouter, the hosts whose terms were checked; otherwise the cheapest host on OpenRouter. Brackets give the quantisation a host states; it may differ from what the boards ran.
- **Memory rebuild** = 605 × (1,503 × in + 161 × out); **thread turn** = 1.91 × (3,274 × in + 189 × out). Thinking tokens bill as output and aren't included.
- **Weights:** "open" means published weights whose licence wasn't checked.

| Model | Vectara % (answered) | FACTS G. % | AA-Omni % | $/M in / out | Seller | Memory rebuild | Thread turn | Context | Weights | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| Ant Group finix_s1_32b | 1.8 (99.5) |  |  |  | none |  |  | – | not released | no API or weights (HF 401; Vectara issue #143) |
| OpenAI gpt-5.4-nano (2026-03-17) | 3.1 (100.0) |  |  | 0.2 / 1.25 | OpenAI | $0.30 | $0.0017 | 400k (272k in) | closed | deprecated 2026-10-01, shutdown 2027-04-01 (→ gpt-6-luna); effort none–xhigh, default none |
| Google Gemini 2.5 Flash-Lite | 3.3 (99.5) |  |  | 0.1 / 0.4 | Google | $0.13 | $0.0008 | 1M | closed | 2.5 models open only to past users; thinking off by default; no shutdown date |
| Microsoft Phi-4 | 3.7 (80.7) |  |  | 0.056 / 0.112 | DeepInfra (bf16) | $0.06 | $0.0004 | 16k | MIT | context below the 21k-token largest thread input; FaithJudge 17.0% |
| Meta Llama 3.3 70B Instruct | 4.1 (99.5) |  |  | 0.1 / 0.32 | DeepInfra (fp8) | $0.12 | $0.0007 | 131k | Llama 3.3 | Vectara ran Together's Turbo build; FaithJudge 16.4% |
| DeepSeek V4.1 Flash | 4.2 (99.9) |  | 66.8 | 0.3 / 1.2 | DeepSeek | $0.39 | $0.0023 | 1M | MIT (763B) | off-peak 0.15/0.60; API serves the 1017 snapshot, Vectara row names none; non-thinking mode available, thinking default; DeepInfra 0.078/0.235 (1017, fp8) |
| Snowflake Arctic Instruct | 4.3 (62.7) |  |  |  | not checked |  |  | – | Apache-2.0 (479B) | not on OpenRouter or HF providers; Vectara ran it on Replicate |
| Google Gemma 3 12B | 4.4 (97.4) | 58.3 |  | 0.04 / 0.12 | DeepInfra (bf16) | $0.05 | $0.0003 | 131k | Gemma terms |  |
| Mistral Large 2411 | 4.5 (99.9) |  |  |  | – |  |  | – | – | retired by Mistral 2026-05-31 |
| Qwen3 8B | 4.8 (99.9) |  |  |  | not checked |  |  | – | Apache-2.0 | not on OpenRouter; HF providers Featherless, Nscale |
| Amazon Nova Pro | 5.1 (99.3) |  |  | 0.8 / 3.2 | Amazon Bedrock | $1.04 | $0.0062 | 300k | closed |  |
| Amazon Nova 2 Lite | 5.1 (99.6) |  |  | 0.3 / 2.5 | Amazon Bedrock | $0.52 | $0.0028 | 1M | closed |  |
| Mistral Small 2501 | 5.1 (97.9) |  |  | 0.05 / 0.08 | DeepInfra (fp8) | $0.05 | $0.0003 | 32k | Apache-2.0 | retired on Mistral's API 2025-11-30; FaithJudge 17.0% |
| IBM Granite 4.0 H Small | 5.2 (100.0) |  |  |  | not checked |  |  | – | Apache-2.0 (32B) | not on OpenRouter or HF providers; Vectara ran it on Replicate |
| Google Gemma 4 26B A4B | 5.2 (99.8) | 80.9 |  | 0.07 / 0.34 | DeepInfra (fp8) | $0.10 | $0.0006 | 262k | open |  |
| AI21 Jamba Mini 2 | 5.3 (99.6) |  |  | 0.2 / 0.4 | AI21 | $0.22 | $0.0014 | – | – | AI21 prices "Jamba Mini" without a version |
| DeepSeek V3.2-Exp | 5.3 (96.6) |  |  | 0.27 / 0.41 | SiliconFlow (fp8) | $0.29 | $0.0018 | 164k | open |  |
| Qwen3 14B | 5.4 (99.9) |  |  | 0.12 / 0.24 | DeepInfra (fp8) | $0.13 | $0.0008 | 41k | Apache-2.0 |  |
| Amazon Nova Micro | 5.5 (100.0) |  |  | 0.035 / 0.14 | Amazon Bedrock | $0.05 | $0.0003 | 128k | closed |  |
| DeepSeek V3.1 | 5.5 (94.5) | 57.4 |  | 0.25 / 0.95 | DeepInfra (fp4) | $0.32 | $0.0019 | 164k | open |  |
| OpenAI gpt-5.4-mini (2026-03-17) | 5.5 (100.0) |  |  | 0.75 / 4.5 | OpenAI | $1.12 | $0.0063 | 400k | closed |  |
| OpenAI gpt-4.1 | 5.6 (99.9) | 45.6 |  | 2 / 8 | OpenAI | $2.60 | $0.0154 | 1M | closed | shutdown 2027-01-14 (→ gpt-6-luna); FaithJudge 11.9% |
| Qwen3 4B | 5.7 (99.9) |  |  |  | not checked |  |  | – | Apache-2.0 | not on OpenRouter; HF provider Featherless |
| xAI Grok 3 | 5.8 (93.0) |  |  |  | not checked |  |  | – | closed | not on OpenRouter; FaithJudge 15.3% |
| Qwen3 32B | 5.9 (99.9) |  |  | 0.08 / 0.28 | DeepInfra (fp8) | $0.10 | $0.0006 | 41k | Apache-2.0 |  |
| Amazon Nova Lite | 6.1 (99.9) |  |  | 0.06 / 0.24 | Amazon Bedrock | $0.08 | $0.0005 | 300k | closed |  |
| DeepSeek V3 | 6.1 (97.5) | 33.4 |  | 0.256 / 0.712 | DeepInfra (fp4) | $0.30 | $0.0019 | 164k | open | OpenRouter "deepseek-chat"; snapshot not confirmed |
| DeepSeek V3.2 | 6.3 (92.6) |  |  | 0.26 / 0.38 | DeepInfra (fp4) | $0.27 | $0.0018 | 164k | open |  |
| Google Gemma 3 4B | 6.4 (67.3) | 38.4 |  | 0.05 / 0.1 | DeepInfra (bf16) | $0.06 | $0.0003 | 131k | Gemma terms |  |
| OpenAI gpt-6-sol | 6.5 (100.0) | 85.2 |  | 2 / 10 | OpenAI | $2.79 | $0.0161 | 1.05M | closed |  |
| Cohere Command R+ 08-2024 | 6.9 (95.0) |  |  | 2.5 / 10 | Cohere | $3.25 | $0.0192 | 128k | CC-BY-NC |  |
| Arcee Trinity Large Preview | 6.9 (99.0) |  |  |  | not checked |  |  | – | other (399B) | not on OpenRouter |
| OpenAI gpt-5.4 | 7.0 (99.9) | 70.3 |  | 2.5 / 15 | OpenAI | $3.73 | $0.0210 | 1.05M | closed |  |
| Google Gemini 2.5 Pro | 7.0 (99.1) | 74.3 |  | 1.25 / 10 | Google | $2.11 | $0.0114 | 1M | closed | 2.5 models open only to past users; FaithJudge 6.7% |
| Mistral Ministral 3B 2410 | 7.3 (99.9) |  |  |  | – |  |  | – | – | retired by Mistral 2025-12-31 |
| Google Gemma 3 27B | 7.4 (98.8) | 56.4 |  | 0.08 / 0.16 | DeepInfra (fp8) | $0.09 | $0.0006 | 131k | Gemma terms |  |
| Google Gemma 4 31B | 7.4 (100.0) | 80.7 |  | 0.135 / 0.51 | DeepInfra (fp4) | $0.17 | $0.0010 | 262k | open |  |
| Mistral Ministral 8B 2410 | 7.4 (99.9) |  |  |  | – |  |  | – | – | retired by Mistral 2025-12-31; FaithJudge 30.9% |
| Meta Llama 4 Scout | 7.7 (99.0) |  |  | 0.08 / 0.24 | DeepInfra (fp8) | $0.10 | $0.0006 | 328k | Llama 4 |  |
| Google Gemini 2.5 Flash | 7.8 (99.0) | 70.0 |  | 0.3 / 2.5 | Google | $0.52 | $0.0028 | 1M | closed | 2.5 models open only to past users; FaithJudge 6.3% |
| Meta Llama 4 Maverick | 8.2 (100.0) |  |  | 0.1875 / 0.6525 | DigitalOcean | $0.23 | $0.0014 | 128k | Llama 4 | FaithJudge 20.6% |
| Google Gemini 3.1 Flash-Lite | 8.2 (99.6) | 66.5 |  | 0.25 / 1.5 | Google | $0.37 | $0.0021 | 1M | closed | Vectara and FACTS ran the preview; shutdown 2027-05-07 (→ 3.5 Flash-Lite) |
| OpenAI gpt-5.4-pro | 8.3 (100.0) |  |  | 30 / 180 | OpenAI | $44.81 | $0.2526 | 1.05M | closed |  |
| OpenAI gpt-5.2 (low effort) | 8.4 (100.0) | 76.2 |  | 1.75 / 14 | OpenAI | $2.95 | $0.0160 | 400k | closed | price from OpenRouter's OpenAI endpoint; high effort: Vectara 10.8% |
| DeepSeek V4 Pro | 8.6 (97.2) |  |  | 1.32 / 3.96 | DeepSeek | $1.59 | $0.0097 | 1M | open | off-peak 0.66/1.98 |
| OpenAI gpt-6-astra | 8.7 (100.0) | 82.0 | 51.3 | 10 / 50 | OpenAI | $13.96 | $0.0806 | 1.05M | closed | reasoning cannot be set to none |
| MiniMax M2.5 | 9.1 (98.2) |  |  | 0.3 / 1.2 | MiniMax | $0.39 | $0.0023 | 205k | other |  |
| Cohere Command A 03-2025 | 9.3 (97.6) |  |  | 2.5 / 10 | Cohere | $3.25 | $0.0192 | 256k | CC-BY-NC |  |
| OpenAI gpt-5.5 | 9.3 (100.0) | 76.2 |  | 5 / 30 | OpenAI | $7.47 | $0.0421 | 1.05M | closed | effort includes none, default medium |
| Z.AI GLM-4.5-Air | 9.3 (98.1) |  |  | 0.2 / 1.1 | Z.AI | $0.29 | $0.0016 | 131k | MIT | Vectara ran the FP8 weights |
| Qwen3 235B A22B (2504) | 9.3 (94.9) |  |  |  | – |  |  | – | Apache-2.0 | not on OpenRouter; the 2507 instruct is a different model (FACTS 40.8%) |
| Qwen3 Next 80B A3B Thinking | 9.3 (94.4) |  |  | 0.15 / 1.2 | Google Vertex | $0.25 | $0.0014 | 262k | Apache-2.0 |  |
| Z.AI GLM-4.7 Flash | 9.3 (91.6) |  |  | 0.0605 / 0.4 | Cloudflare | $0.09 | $0.0005 | 131k | MIT |  |
| Cohere Aya Expanse 8B | 9.5 (77.5) |  |  |  | Cohere |  |  | – | CC-BY-NC | price not checked |
| Z.AI GLM-4.6 | 9.5 (94.5) |  |  | 0.6 / 2.2 | Z.AI | $0.76 | $0.0045 | 203k | MIT |  |
| NVIDIA Nemotron 3 Nano 30B A3B | 9.6 (99.6) |  |  | 0.05 / 0.2 | Novita (fp4) | $0.06 | $0.0004 | 262k | NVIDIA open |  |
| OpenAI gpt-4o (2024-08-06) | 9.6 (93.8) |  |  | 2.5 / 10 | OpenAI | $3.25 | $0.0192 | 128k | closed | shutdown 2027-01-14 |
| AI21 Jamba Large 1.7 | 9.7 (98.9) |  |  | 2 / 8 | AI21 | $2.60 | $0.0154 | – | other | AI21 prices "Jamba Large" without a version |
| Anthropic Claude Haiku 4.5 | 9.8 (99.5) |  |  | 1 / 5 | Anthropic | $1.40 | $0.0081 | 200k | closed |  |
| Anthropic Claude Opus 5.5 | 9.9 (99.7) | 68.5 | 58.6 | 4 / 20 | Anthropic | $5.59 | $0.0322 | 1M | closed |  |
| Z.AI GLM-5 | 10.1 (99.7) |  |  | 1 / 3.2 | Z.AI | $1.22 | $0.0074 | 203k | open |  |
| Anthropic Claude Sonnet 4 | 10.3 (98.6) | 56.1 |  | 3 / 15 | Amazon Bedrock | $4.19 | $0.0242 | – | closed | retired on Anthropic's API; Bedrock and Google Cloud only |
| Google Gemini 3.1 Pro (preview) | 10.4 (99.4) | 65.7 |  | 2 / 12 | Google | $2.99 | $0.0168 | 1M | closed | thinking low–high, default high |
| Qwen3.5 Flash (2026-02-23) | 10.5 (99.8) |  |  | 0.065 / 0.26 | Alibaba | $0.08 | $0.0005 | 1M | closed |  |
| Qwen3.5 35B A3B | 10.5 (99.8) |  |  | 0.1625 / 1.3 | Alibaba | $0.27 | $0.0015 | 262k | open |  |
| OpenAI gpt-5-nano | 10.5 (100.0) |  |  | 0.05 / 0.4 | OpenAI | $0.08 | $0.0005 | 400k | closed | shutdown 2026-12-11 (→ gpt-5.6-luna) |
| Anthropic Claude Sonnet 4.6 | 10.6 (99.9) | 56.2 |  | 3 / 15 | Anthropic | $4.19 | $0.0242 | 1M | closed |  |
| IBM Granite 3.3 8B Instruct | 10.6 (100.0) |  |  |  | not checked |  |  | – | Apache-2.0 | not on OpenRouter; Vectara ran it on Replicate |
| Qwen3.5 Plus (2026-02-15) | 10.7 (99.8) |  |  | 0.26 / 1.56 | Alibaba | $0.39 | $0.0022 | 1M | closed |  |
| Moonshot Kimi K2.6 | 10.8 (99.7) |  |  | 0.95 / 4 | Moonshot | $1.25 | $0.0074 | 262k | other |  |
| Anthropic Claude Opus 4.5 | 10.9 (98.7) | 62.1 |  | 5 / 25 | Anthropic | $6.98 | $0.0403 | 200k | closed |  |
| Cohere Aya Expanse 32B | 10.9 (99.8) |  |  |  | Cohere |  |  | – | CC-BY-NC | price not checked |
| OpenAI gpt-5.1 (low effort) | 10.9 (100.0) | 50.0 |  | 1.25 / 10 | OpenAI | $2.11 | $0.0114 | 400k | closed | shutdown 2027-04-01; high effort: Vectara 12.1% |
| Qwen3.5 122B A10B | 11.2 (99.8) |  |  | 0.26 / 2.08 | Alibaba | $0.44 | $0.0024 | 262k | open |  |
| DeepSeek R1 (2501) | 11.3 (97.0) | 56.0 |  |  | – |  |  | – | MIT | original not on OpenRouter; R1-0528 on DeepInfra 0.50/2.15 (FaithJudge 9.8%) |
| Z.AI GLM-4.7 | 11.7 (99.8) |  |  | 0.6 / 2.2 | Z.AI | $0.76 | $0.0045 | 203k | MIT |  |
| Anthropic Claude Opus 4.1 | 11.8 (92.4) | 54.8 |  | 15 / 75 | Amazon Bedrock | $20.95 | $0.1209 | 200k | closed | retired on Anthropic's API |
| MiniMax M2.1 | 11.8 (98.5) |  |  | 0.3 / 1.2 | MiniMax | $0.39 | $0.0023 | 205k | other |  |
| Anthropic Claude Opus 4 | 12.0 (91.0) |  |  | 15 / 75 | Google Vertex | $20.95 | $0.1209 | – | closed | retired except on Google Cloud |
| Anthropic Claude Sonnet 4.5 | 12.0 (95.6) | 58.7 |  | 3 / 15 | Anthropic | $4.19 | $0.0242 | 1M | closed | FACTS with thinking 61.8% |
| Anthropic Claude Opus 4.7 | 12.0 (98.0) | 61.7 |  | 5 / 25 | Anthropic | $6.98 | $0.0403 | 1M | closed |  |
| Qwen3.5 27B | 12.1 (99.8) |  |  | 0.195 / 1.56 | Alibaba | $0.33 | $0.0018 | 262k | open |  |
| Google Gemini 4 Argon | 12.1 (99.1) | 78.6 | 15.1 | 2 / 10 | Google | $2.79 | $0.0161 | – | closed | 4.00/20.00 from 2026-12-01; thinking low–high |
| Anthropic Claude Opus 4.6 | 12.2 (99.8) | 54.9 |  | 5 / 25 | Anthropic | $6.98 | $0.0403 | 1M | closed |  |
| Inception Mercury 2 | 12.3 (100.0) |  |  | 0.25 / 0.75 | Inception | $0.30 | $0.0018 | 128k | closed |  |
| OpenAI gpt-5.6-sol | 12.4 (99.1) | 68.7 |  | 4 / 20 | OpenAI | $5.59 | $0.0322 | 1.05M | closed | promotional price "at least through November 21, 2026" |
| MiniMax M2.7 | 12.9 (99.4) |  |  | 0.3 / 1.2 | MiniMax | $0.39 | $0.0023 | 205k | other |  |
| OpenAI gpt-5-mini | 12.9 (99.9) | 58.3 |  | 0.25 / 2 | OpenAI | $0.42 | $0.0023 | 400k | closed | shutdown 2026-12-11 (→ gpt-5.6-terra) |
| Google Gemini 3 Flash (preview) | 13.5 (99.8) | 59.0 |  | 0.5 / 3 | Google | $0.75 | $0.0042 | 1M | closed | price from OpenRouter; not on Google's pricing page; → 3.6 Flash |
| Google Gemini 3 Pro (preview) | 13.6 (99.4) |  |  |  | – |  |  | – | closed | not on Google's pricing page or OpenRouter |
| Moonshot Kimi K2.5 | 14.2 (92.2) |  |  | 0.49 / 2.5 | AtlasCloud (int4) | $0.69 | $0.0040 | 262k | other | no Moonshot endpoint on OpenRouter |
| OpenAI gpt-oss-120b | 14.2 (99.9) | 30.7 |  | 0.037 / 0.17 | DeepInfra (bf16) | $0.05 | $0.0003 | 131k | Apache-2.0 |  |
| Mistral Large 3 (2512) | 14.5 (98.8) |  |  | 0.5 / 1.5 | Mistral | $0.60 | $0.0037 | 262k | Apache-2.0 |  |
| AI21 Jamba Mini 1.7 | 14.7 (99.1) |  |  | 0.2 / 0.4 | AI21 | $0.22 | $0.0014 | – | other | AI21 prices "Jamba Mini" without a version |
| OpenAI gpt-5 (minimal effort) | 14.7 (99.9) | 69.6 |  | 1.25 / 10 | OpenAI | $2.11 | $0.0114 | 400k | closed | shutdown 2026-12-11; high effort: Vectara 15.1% |
| xAI Grok 4.1 Fast (non-reasoning) | 17.8 (98.5) |  |  |  | not checked |  |  | – | closed | not on OpenRouter; reasoning: Vectara 19.2% |
| Moonshot Kimi K2 Instruct 0905 | 17.9 (98.6) |  |  | 0.6 / 2.5 | Novita (fp8) | $0.79 | $0.0047 | 262k | other |  |
| OpenAI o4-mini | 18.6 (99.2) | 29.3 |  |  | – |  |  | – | closed | shut down 2026-10-23 |
| Mistral Ministral 3 14B (2512) | 19.4 (99.6) |  |  | 0.2 / 0.2 | Mistral | $0.20 | $0.0013 | 262k | Apache-2.0 |  |
| xAI Grok 4 Fast (non-reasoning) | 19.7 (99.2) |  |  |  | not checked |  |  | – | closed | not on OpenRouter; reasoning: Vectara 20.2%, FACTS 43.1% |
| Mistral Ministral 3 8B (2512) | 21.7 (99.1) |  |  | 0.15 / 0.15 | Mistral | $0.15 | $0.0010 | 262k | Apache-2.0 |  |
| Mistral Medium 3.1 (2508) | 22.7 (99.7) |  |  | 0.4 / 2 | Mistral | $0.56 | $0.0032 | 131k | closed | Mistral docs: retired 2026-08-31; OpenRouter still lists a Mistral endpoint |
| OpenAI o3-pro | 23.3 (100.0) |  |  | 20 / 80 | OpenAI | $25.98 | $0.1539 | 200k | closed | shutdown 2026-12-11 |
| Microsoft Phi-4-mini-instruct | 23.5 (92.5) |  |  |  | not checked |  |  | – | MIT | not on OpenRouter; HF provider Featherless; FaithJudge 38.4% |
| Mistral Ministral 3 3B (2512) | 24.2 (74.3) |  |  | 0.1 / 0.1 | Mistral | $0.10 | $0.0007 | 131k | Apache-2.0 |  |
| *Not on Vectara's board* | | | | | | | | | | |
| Anthropic Claude Sonnet 5.5 (today) |  |  | 47.0 | 2 / 10 | Anthropic | $2.79 | $0.0161 | 1M | closed | today's summariser, through claude -p on the subscription |
| Anthropic Claude Haiku 5.5 |  | 65.8 | 40.4 | 0.1 / 0.5 | Anthropic | $0.14 | $0.0008 | 1M | closed | failed our 2026-10-08 eval against Sonnet 5.5 (distortions when merging); 0.50/2.50 over 100k prompt |
| Anthropic Claude Opus 4.8 |  | 69.7 |  | 5 / 25 | Anthropic | $6.98 | $0.0403 | 1M | closed |  |
| Anthropic Claude Fable 5.1 |  |  | 72.6 | 10 / 50 | Anthropic | $13.96 | $0.0806 | 1M | closed |  |
| OpenAI gpt-6-luna |  |  | 76.7 | 0.1 / 0.5 | OpenAI | $0.14 | $0.0008 | 1.05M (922k in) | closed | effort none–max, default medium; AA ran it at max |
| OpenAI gpt-6.1-sol |  | 82.4 | 54.3 | 2 / 10 | OpenAI | $2.79 | $0.0161 | 1.05M | closed | effort cannot be none or minimal |
| OpenAI gpt-6.1-terra |  |  | 38.0 | 1 / 5 | OpenAI | $1.40 | $0.0081 | 1.05M | closed | AA ran it at max |
| OpenAI gpt-5.6-luna |  |  |  | 0.2 / 1.2 | OpenAI | $0.30 | $0.0017 | 1.05M (922k in) | closed | effort none–max, default medium |
| OpenAI gpt-5.6-terra |  | 71.2 |  | 2 / 12 | OpenAI | $2.99 | $0.0168 | 1.05M | closed |  |
| OpenAI gpt-oss-20b |  | 23.1 |  | 0.03 / 0.14 | DeepInfra (bf16) | $0.04 | $0.0002 | 131k | Apache-2.0 |  |
| Google Gemini 3.5 Flash-Lite |  | 71.2 |  | 0.3 / 2.5 | Google | $0.52 | $0.0028 | 1M | closed | thinking minimal–high, default minimal |
| Google Gemini 3.5 Flash |  | 73.2 |  | 0.75 / 4.5 | Google | $1.12 | $0.0063 | 1M | closed | price from OpenRouter; not on Google's pricing page |
| Google Gemini 3.6 Flash |  | 72.5 |  | 0.75 / 3.75 | Google | $1.05 | $0.0060 | 1M | closed | 1.50/7.50 from 2027-01-01; thinking minimal–high, default medium |
| Google Gemini 3.7 Flash |  | 75.5 |  | 0.75 / 3.75 | Google | $1.05 | $0.0060 | 1M | closed | price from OpenRouter; not on Google's pricing page |
| Google Gemini 3.8 Flash |  | 73.1 | 55.2 | 0.75 / 3.75 | Google | $1.05 | $0.0060 | 1M | closed | 1.50/7.50 from 2027-01-01; thinking low–high, default medium |
| Qwen3.7 Flash |  |  |  | 0.03 / 0.13 | Alibaba | $0.04 | $0.0002 | 1M | closed |  |
| Qwen3.6 Flash |  |  |  | 0.1875 / 1.125 | Alibaba | $0.28 | $0.0016 | 1M | closed |  |
| Qwen3.8 Flash |  |  |  | 0.15 / 0.47 | Alibaba | $0.18 | $0.0011 | 1M | closed |  |
| Qwen3.8 27B |  |  | 30.3 | 0.425 / 2.55 | Alibaba | $0.63 | $0.0036 | 1M | open | AA ran it at xhigh |
| Qwen3.8 Max (0902) |  |  | 28.8 | 2 / 6 | Alibaba | $2.40 | $0.0147 | 1M | closed | price from OpenRouter listing |
| Z.AI GLM-5.3 Flash |  |  |  | 0.15 / 0.5 | Z.AI | $0.19 | $0.0011 | 1M | open |  |
| Z.AI GLM-5.3 |  |  | 29.6 | 1.4 / 4.4 | Z.AI | $1.70 | $0.0103 | 1M | open | AA ran it at max |
| MiniMax M3.1 Flash |  |  | 92.8 | 0.15 / 0.6 | MiniMax | $0.19 | $0.0012 | 1M | open |  |
| MiniMax M3 |  |  |  | 0.3 / 1.2 | MiniMax | $0.39 | $0.0023 | 524k | open |  |
| Moonshot Kimi K3 |  |  |  | 3 / 15 | Moonshot | $4.19 | $0.0242 | 1M | open |  |
| Moonshot Kimi K3.1 Pro |  |  | 43.9 | 2 / 10 | OpenRouter listing | $2.79 | $0.0161 | 1M | – | seller not checked |
| Mistral Small 4 (2603) |  |  |  | 0.15 / 0.6 | Mistral | $0.19 | $0.0012 | 262k | Apache-2.0 |  |
| Mistral Medium 3.5 |  |  |  | 1.5 / 7.5 | Mistral | $2.09 | $0.0121 | 262k | modified MIT |  |
| Mistral Large 4 |  |  | 41.9 | 1.36 / 4.18 | Mistral | $1.64 | $0.0100 | 1M | open | AA ran the preview |
| IBM Granite 4.2 8B |  |  |  | 0.06 / 0.25 | DeepInfra (bf16) | $0.08 | $0.0005 | 131k | open |  |
| NVIDIA Nemotron 3.5 Super VL |  |  | 35.8 | 0.15 / 0.5 | Together | $0.19 | $0.0011 | 262k | NVIDIA open |  |
| Inception Mercury 3 |  |  |  | 0.1 / 0.35 | Inception | $0.13 | $0.0008 | 1M | closed | OpenRouter marks the price discounted |
| Cohere Command A Plus |  |  |  | 0.3 / 1.5 | Cohere | $0.42 | $0.0024 | 192k | – |  |
| Cohere Command R7B (12-2024) |  |  |  | 0.0375 / 0.15 | Cohere | $0.05 | $0.0003 | 128k | CC-BY-NC |  |
| xAI Grok 4.7 |  |  | 29.3 | 2 / 6 | xAI | $2.40 | $0.0147 | 500k | closed | AA ran it at xhigh; Grok 4.5 FACTS 74.5% |
| Meta Llama 3.1 8B Instruct |  |  |  | 0.02 / 0.04 | DeepInfra (fp8) | $0.02 | $0.0001 | 131k | Llama 3.1 | FaithJudge 28.4% |
| Meta Muse Spark Open |  |  | 24.1 | 1.25 / 4.25 | OpenRouter listing | $1.55 | $0.0094 | – | open | seller not checked |
| Meta Muse Wonder |  |  | 26.5 | 4 / 20 | OpenRouter listing | $5.59 | $0.0322 | – | closed | seller not checked; Muse Wonder Full 23.0% (not on OpenRouter) |
| StepFun Step 5 Preview |  |  | 43.0 | 1 / 2.7 | OpenRouter listing | $1.17 | $0.0072 | – | – | seller not checked |
| Xiaomi MiMo-V2.6-Pro |  |  | 40.6 | 0.435 / 0.87 | OpenRouter listing | $0.48 | $0.0030 | – | – | seller not checked |
| Reflection Beam 1.0 |  |  | 47.2 | 0.25 / 1 | OpenRouter listing | $0.32 | $0.0019 | – | open (501B) | seller not checked |
| Thinking Machines Inkling |  |  | 67.7 | 1 / 4.05 | OpenRouter listing | $1.30 | $0.0077 | – | – | seller not checked |
| K2 Horizon 375B A23B (Institute of Foundation Models) |  |  | 25.9 |  | not checked |  |  | – | – | not on OpenRouter |
| DISLab SummLlama3.1-8B / 70B |  |  |  |  | self-host |  |  | 128k (Llama 3.1) | Llama 3.1 (card states none) | no API host; FineSurE faithfulness 0.924 (8B), 0.950 (SummLlama3-70B), GPT-4o 0.940 |

## Sellers' data terms

| Seller | Based / data location | Trains on API data | Retention | `genai` 0.7.1 adapter | Source (date) |
|---|---|---|---|---|---|
| Anthropic API | US | no by default; feedback excepted | deleted within 30 days; flagged content up to 2 years | `anthropic` | [retention](https://privacy.claude.com/en/articles/7996866) (2026-07-01), [training](https://privacy.claude.com/en/articles/7996868) |
| Anthropic subscription via `claude -p` (today) | US | consumer terms, not checked | not checked | none | – |
| OpenAI | US; regional storage in 10 regions | no unless opted in | abuse logs up to 30 days; zero retention on approval | `openai`, `openai_resp` | [your data](https://developers.openai.com/api/docs/guides/your-data) (undated) |
| Google Gemini API, paid tier | US; may be stored in any country with Google facilities | no | prompts logged "for a limited period" for abuse and legal disclosure | `gemini`, `vertex` | [terms](https://ai.google.dev/gemini-api/terms) (2026-04-28) |
| Google Gemini API, free tier | same | yes; human reviewers may read | not stated | same | same |
| DeepSeek | China (PRC) | Open Platform terms silent; privacy policy lists training on input to its Services, with a right to opt out | while the account exists | `deepseek` | [privacy](https://cdn.deepseek.com/policies/en-US/deepseek-privacy-policy.html) (2026-02-10), [Open Platform terms](https://cdn.deepseek.com/policies/en-US/deepseek-open-platform-terms-of-service.html) (effective 2026-04-29) |
| Alibaba Model Studio | Singapore; datacentres SG, CN (OpenRouter) | "will never use your data for model training" | stores call data; period not stated | `aliyun`, `qwen_cloud` | [privacy notice](https://www.alibabacloud.com/help/en/model-studio/privacy-notice) (2026-10-11) |
| Z.AI | Singapore | not stated for API content | DPA: API content not saved, held temporarily to serve | `zai` | [privacy, DPA](https://docs.z.ai/legal-agreement/privacy-policy) (2025-09-29) |
| Moonshot (Kimi) | Singapore | yes, "training and refining our underlying technology" | while the account is active | `moonshot`, `kimi` | [privacy](https://platform.kimi.ai/docs/agreement/userprivacy) (2025-04-30) |
| MiniMax | Singapore; datacentre US (OpenRouter) | not checked: the policy page didn't render | not checked | `minimax` | – |
| Mistral | France | paid plans: may opt out; free mode: may train | not checked | `mistral` | [help](https://help.mistral.ai/en/articles/347617-do-you-use-my-user-data-to-train-your-artificial-intelligence-models) (undated) |
| Amazon Bedrock | US | not stated on the page read; model providers have no access to prompts or completions | not checked | `bedrock_api`, `bedrock_sigv4` | [data protection](https://docs.aws.amazon.com/bedrock/latest/userguide/data-protection.html) |
| Together | US | no without opt-in | zero retention by setting | `together` | [privacy](https://www.together.ai/privacy) (2025-12-17) |
| DeepInfra | US | no without consent | "do not store this data" | `genai_n` (OpenAI-compatible) | [privacy](https://deepinfra.com/privacy) (2026-08-15) |
| Fireworks | US | no without opt-in | no prompt logging for open models without opt-in | `fireworks` | [privacy](https://fireworks.ai/privacy-policy) (2026-09-16) |
| Groq | US | not stated | none by default; up to 30 days for troubleshooting or abuse unless zero retention is on | `groq` | [your data](https://console.groq.com/docs/your-data) (undated) |
| Cohere (US), AI21 (IL), xAI (US), Inception, Cloudflare (US), DigitalOcean (US), Novita (US), SiliconFlow (SG; US datacentre), AtlasCloud (US), Google Vertex for third-party models | per [OpenRouter's provider list](https://openrouter.ai/api/v1/providers) | not checked | not checked | `cohere`, `xai`; others `genai_n` | – |
| OpenRouter itself | – | its [provider-logging page](https://openrouter.ai/docs/guides/privacy/provider-logging) doesn't state its own retention | – | `open_router` | – |

## Reasoning

Turning reasoning off, from the providers' docs:
- **OpenAI:**
  - gpt-5.4-nano takes none to xhigh, default none.
  - gpt-6-luna and gpt-5.6-luna take none to max, default medium.
  - gpt-5.5 takes none, default medium.
  - gpt-6.1-sol refuses none and minimal; gpt-6-astra refuses none.
  - Others not checked.
- **Google** ([thinking](https://ai.google.dev/gemini-api/docs/thinking)):
  - 2.5 Flash-Lite: thinking off by default.
  - 3.5 Flash-Lite: minimal to high, default minimal.
  - 3.6 Flash: minimal to high, default medium.
  - 3.8 Flash: low to high, default medium.
  - 3.1 Pro: low to high, default high.
  - 4 Argon: low to high, default medium.
  - The page doesn't cover 3.1 Flash-Lite or 3.7 Flash.
- **DeepSeek** `deepseek-flash` and `deepseek-v4-pro`: thinking and non-thinking modes, thinking by default.
- **Sonnet 5.5, our calls:** zero thinking tokens in every record, at medium effort.
- Other providers not checked.

What the boards show when only the effort changes:
- **Vectara:**
  - gpt-5: minimal 14.7%, high 15.1%.
  - gpt-5.1: low 10.9%, high 12.1%.
  - gpt-5.2: low 8.4%, high 10.8%.
  - o4-mini: 18.6% at both low and high.
  - Grok 4 Fast: 19.7% without reasoning, 20.2% with.
  - Grok 4.1 Fast: 17.8% without reasoning, 19.2% with.
- **FACTS Grounding:** Sonnet 4.5 scores 58.7% without thinking, 61.8% with it.
- **Confabulation board:** Claude Sonnet 4 confabulates 5.4% without thinking, 2.5% with it.

Vectara's API section stops at 2025 models, so the board doesn't say what effort its newer rows used.

## Benchmarks

### Vectara hallucination leaderboard

[README](https://github.com/vectara/hallucination-leaderboard), updated 2026-10-22; that update added DeepSeek-V4.1-Flash and claude-opus-5-5.
- **What it measures:** whether a summary is consistent with its source, judged by HHEM-2.3, Vectara's commercial classifier.
- **Data:** over 7,700 private English articles of 50 to 24,000 words: news, technology, science, medicine, legal, sports, business and education.
- **Prompt:** asks for a "concise and factual summary", "only the information in the given passage", and says summaries "should never exceed 20 percent of the original text's length".
- **Summary lengths:** averages run from 54.7 words (gpt-5.4-mini) to 420.2 words (Phi-4-mini).
- **Anthropic models listed:** Haiku 4.5 9.8%, Opus 5.5 9.9%, Sonnet 4 10.3%, Sonnet 4.6 10.6%, Opus 4.5 10.9%, Opus 4.1 11.8%, Opus 4 12.0%, Sonnet 4.5 12.0%, Opus 4.7 12.0%, Opus 4.6 12.2%. Sonnet 5.5 and Haiku 5.5 aren't listed.
- **Limits for our task:**
  - It scores unsupported content only. Omissions, format, speaker attribution, length ceilings and merge order go unmeasured.
  - Our thread lines compress inputs of up to 21,075 tokens into at most 200 words, far beyond its 20% cap.
  - The classifier itself errs: its open sibling HHEM-2.1-Open has 76.55% balanced accuracy on AggreFact-SOTA ([card](https://huggingface.co/vectara/hallucination_evaluation_model)).

### FACTS Grounding

- **What it measures:** long-form answers grounded in a document of up to about 32,000 tokens, across summarisation, Q&A and rewriting tasks.
- **Data and judging:** 1,719 examples, 860 public and 859 private. Three LLM judges score each answer, and an answer that doesn't address the request fails ([DeepMind](https://deepmind.google/blog/facts-grounding-a-new-benchmark-for-evaluating-the-factuality-of-large-language-models/), 2024-12-17, describing v1). The board now runs "Kaggle benchmark v2; task revision 3"; the v2 changes weren't checked.
- **Source:** [Kaggle's board](https://www.kaggle.com/benchmarks/google/facts-grounding) renders only with JavaScript. The figures come from [BenchLM's mirror](https://benchlm.ai/benchmarks/facts-grounding) (2026-10-13), which attributes them to Kaggle.
- **Anthropic models:** Haiku 5.5 65.8 and Opus 5.5 68.5. Sonnet 5.5 isn't listed.
- **All 55 rows:** GPT-6 Sol 85.2; GPT-6.1 Sol 82.4; GPT-6 Astra 82.0; Gemma 4 26B A4B 80.9; Gemma 4 31B 80.7; Gemini 4 Argon 78.6; GPT-5.2 76.2; GPT-5.5 76.2; Gemini 3.7 Flash 75.5; Grok 4.5 74.5; Gemini 2.5 Pro 74.3; Gemini 3.5 Flash 73.2; Gemini 3.8 Flash 73.1; Gemini 3.6 Flash 72.5; Llama 3 Grounded Language Model 71.8; GPT-5.6 Terra 71.2; Gemini 3.5 Flash-Lite 71.2; GPT-5.4 70.3; Gemini 2.5 Flash 70.0; Claude Opus 4.8 69.7; GPT-5 69.6; GPT-5.6 Sol 68.7; Claude Opus 5.5 68.5; Gemini 3.1 Flash-Lite Preview 66.5; Claude Haiku 5.5 65.8; Gemini 3.1 Pro Preview 65.7; Claude Opus 4.5 62.1; Claude Sonnet 4.5 (thinking) 61.8; Claude Opus 4.7 61.7; Gemini 3 Flash Preview 59.0; Claude Sonnet 4.5 58.7; Gemma 3 12B 58.3; GPT-5 mini 58.3; DeepSeek V3.1 57.4; Gemma 3 27B 56.4; Claude Sonnet 4.6 56.2; Claude Sonnet 4 56.1; DeepSeek-R1 56.0; Claude Opus 4.6 54.9; Claude Opus 4.1 54.8; Grok 4 54.7; GPT-5.1 50.0; Mistral Large 2 49.7; Qwen 3 235B A22B Thinking 48.2; GPT-4.1 45.6; Grok 3 Mini 45.6; Mistral Small 3.1 43.3; grok-4-fast-reasoning 43.1; Qwen 3 235B A22B Instruct 2507 40.8; Gemma 3 4B 38.4; o3 36.2; DeepSeek-V3 33.4; gpt-oss-120b 30.7; o4 mini 29.3; gpt-oss-20b 23.1.

### AA-Omniscience

- **What it measures:** closed-book knowledge questions with no source text. The hallucination rate is incorrect ÷ (incorrect + partial + not attempted): how often a model answers wrongly instead of abstaining. It doesn't measure grounded summarisation.
- **The only board found that lists Sonnet 5.5.** Haiku 5.5 rates lower than Sonnet here, while our eval found Haiku distorting more.
- **Rows:** the [page](https://artificialanalysis.ai/evaluations/omniscience)'s embedded data (2026-10-26) carries rates for these 27 models, each at the effort named: Gemini 4 Argon (High) 15.1% (accuracy 49.9%, index 42.4); Muse Wonder Full (Max) 23.0% (accuracy 43.2%, index 30.1); Muse Spark Open (Xhigh) 24.1% (accuracy 39.3%, index 24.6); K2 Horizon 375B A23B 25.9% (accuracy 18.2%, index -3.0); Muse Wonder (Max) 26.5% (accuracy 41.1%, index 25.5); Qwen3.8 Max (0902) 28.8% (accuracy 31.7%, index 12.0); Grok 4.7 (Xhigh) 29.3% (accuracy 47.4%, index 32.0); GLM-5.3 (Max) 29.6% (accuracy 33.9%, index 14.3); Qwen3.8 27B (Xhigh) 30.3% (accuracy 15.6%, index -10.0); Nemotron 3.5 Super VL 35.8% (accuracy 23.3%, index -4.2); GPT-6.1 Terra (Max) 38.0% (accuracy 49.1%, index 29.8); Claude Haiku 5.5 (Max) 40.4% (accuracy 36.4%, index 10.7); MiMo-V2.6-Pro 40.6% (accuracy 34.8%, index 8.4); Mistral Large 4 Preview 41.9% (accuracy 25.8%, index -5.3); Step 5 Preview 43.0% (accuracy 41.5%, index 16.4); Kimi K3.1 Pro 43.9% (accuracy 51.0%, index 29.5); Claude Sonnet 5.5 (Max, Default Fallback) 47.0% (accuracy 53.9%, index 32.3); Beam 1.0 47.2% (accuracy 33.1%, index 1.5); GPT-6 Astra (Max) 51.3% (accuracy 62.6%, index 43.4); GPT-6.1 Sol (Max) 54.3% (accuracy 62.1%, index 41.5); Gemini 3.8 Flash (High) 55.2% (accuracy 54.6%, index 29.6); Claude Opus 5.5 (Max, Default Fallback) 58.6% (accuracy 66.2%, index 46.4); DeepSeek V4.1 Flash 1017 (Max) 66.8% (accuracy 42.9%, index 4.7); Inkling (Xhigh) 67.7% (accuracy 41.5%, index 2.0); Claude Fable 5.1 (Max, Default Fallback) 72.6% (accuracy 67.2%, index 43.5); GPT-6 Luna (Max) 76.7% (accuracy 43.8%, index 0.7); MiniMax-M3.1-Flash 92.8% (accuracy 56.2%, index 15.6).

### FaithJudge

[README](https://github.com/vectara/FaithJudge), last updated 2025-11-11, 2025 models only.
- **What it measures:** hallucination in RAG tasks (FaithBench and RagTruth summarisation, RagTruth question answering and data-to-text), judged by an LLM against human-annotated examples.
- **All rows, overall rate (FaithBench summarisation rate):** gemini-2.5-flash 6.26% (FaithBench 19.44%); gemini-2.5-pro 6.65% (FaithBench 25.00%); gemini-2.5-pro-exp-03-25 7.63% (FaithBench 25.00%); r1-0528 9.78% (FaithBench 20.83%); gemini-2.0-flash-001 10.18% (FaithBench 29.17%); o3-mini-medium-2025-01-31 11.55% (FaithBench 33.33%); gpt-4.1-2025-04-14 11.94% (FaithBench 36.11%); gpt-4.5-preview-2025-02-27 11.94% (FaithBench 37.50%); o3-mini-low-2025-01-31 11.94% (FaithBench 33.33%); o3-mini-high-2025-01-31 12.52% (FaithBench 34.72%); claude-opus-4-thinking-20250514 14.09% (FaithBench 38.89%); gpt-3.5-turbo-0125 14.87% (FaithBench 44.44%); grok-3 15.26% (FaithBench 41.67%); gpt-4.1-mini-2025-04-14 15.66% (FaithBench 37.50%); gpt-4o-2024-11-20 15.85% (FaithBench 40.28%); claude-3-7-sonnet-20250219 16.05% (FaithBench 38.89%); claude-3-7-sonnet-thinking-20250219 16.24% (FaithBench 45.83%); Llama-3.3-70B-Instruct 16.44% (FaithBench 44.44%); phi-4 17.03% (FaithBench 44.44%); Mistral-Small-24B-Instruct-2501 17.03% (FaithBench 43.06%); o3-medium-2025-04-16 17.81% (FaithBench 36.11%); claude-sonnet-4-thinking-20250514 18.20% (FaithBench 43.06%); gpt-4o-mini-2024-07-18 18.59% (FaithBench 51.39%); o3-high-2025-04-16 18.59% (FaithBench 43.06%); claude-sonnet-4-20250514 18.59% (FaithBench 48.61%); Qwen2.5-32B-Instruct 19.18% (FaithBench 36.11%); claude-opus-4-20250514 19.57% (FaithBench 45.83%); llama-4-maverick 20.55% (FaithBench 51.39%); o3-low-2025-04-16 20.55% (FaithBench 47.22%); Qwen2.5-72B-Instruct 20.74% (FaithBench 43.06%); QwQ-32B 24.66% (FaithBench 50.00%); glm-4-9b-chat-hf 25.44% (FaithBench 38.89%); o4-mini-medium-2025-04-16 25.83% (FaithBench 44.44%); o4-mini-low-2025-04-16 27.98% (FaithBench 44.44%); Llama-3.1-8B-Instruct 28.38% (FaithBench 44.44%); Qwen2.5-14B-Instruct 28.96% (FaithBench 54.17%); o4-mini-high-2025-04-16 29.94% (FaithBench 54.17%); Ministral-8B-Instruct-2410 30.92% (FaithBench 56.94%); Phi-4-mini-instruct 38.36% (FaithBench 61.11%); Qwen2.5-7B-Instruct 38.55% (FaithBench 62.50%); AI21-Jamba-mini-1.6 39.14% (FaithBench 38.89%); Llama-3.2-3B-Instruct 46.18% (FaithBench 72.22%); Qwen2.5-3B-Instruct 55.97% (FaithBench 69.44%); Qwen2.5-1.5B-Instruct 66.73% (FaithBench 84.72%); Llama-3.2-1B-Instruct 67.71% (FaithBench 75.00%); Qwen2.5-0.5B-Instruct 76.32% (FaithBench 88.89%).

### Confabulation board

[lechmazur/confabulations](https://github.com/lechmazur/confabulations), last updated 2025-08-07.
- **What it measures:** answers invented for 201 misleading questions about provided documents.
- **All rows, confabulation % / non-response %:** GPT-5 (medium reasoning) 10.9/9.8; Gemini 2.5 Pro Preview 05-06 5.9/15.3; Grok 3 Mini Beta (high) 6.9/14.7; Gemini 2.5 Pro Exp 03-25 4.0/17.6; GLM-4.5 7.9/14.7; o1 (medium reasoning) 10.9/12.6; Qwen 3 30B A3B 12.9/11.7; Gemini 2.5 Pro 4.0/20.8; Grok 4 4.0/20.9; Gemini 2.0 Flash Think Exp 01-21 14.9/10.0; DeepSeek R1 17.3/8.0; o1-preview 18.3/7.8; Claude Sonnet 4 Thinking 16K 2.5/23.9; GPT-5 mini (medium reasoning) 21.8/4.8; Gemini 1.5 Pro (Sept) 16.8/10.2; GPT-4.5 Preview 11.9/15.4; Grok 3 Mini Beta (low) 10.9/17.2; Grok 3 Beta (no reasoning) 17.8/10.6; o3-pro (medium reasoning) 23.4/5.1; o3 (high reasoning) 24.8/4.0; DeepSeek R1 05/28 12.9/16.2; Claude 3.7 Sonnet Thinking 16K 7.9/21.5; Claude Sonnet 4 (no reasoning) 5.4/24.2; GPT-4o 2024-08-06 22.3/8.4; Qwen QwQ-32B 16K 25.2/5.9; Qwen 3 235B A22B 23.3/8.0; GPT-OSS-120B (medium reasoning) 23.3/8.0; o4-mini (high reasoning) 26.7/4.8; Claude Opus 4 Thinking 16K 2.5/29.4; Baidu Ernie 4.5 300B A47B 25.2/6.7; GPT-4o Feb 2025 26.7/6.5; Qwen 3 235B A22B 25-07 Think 13.9/19.7; Gemini 2.5 Flash Preview 24K 4.5/29.2; Claude Opus 4 (no reasoning) 3.0/31.2; Claude Opus 4.1 Thinking 16K 2.5/31.7; GPT-4o 2024-11-20 26.2/8.2; Llama 3.1 405B 14.4/20.9; o3-mini (medium reasoning) 27.2/8.6; Gemini 2.0 Pro Exp 02-05 15.8/21.0; o3-mini (high reasoning) 30.7/6.2; Claude Opus 4.1 (no reasoning) 4.0/33.1; o1-mini 26.2/10.9; Qwen 2.5 72B 32.2/6.0; Claude 3.7 Sonnet 25.2/14.3; Claude 3.5 Sonnet 2024-10-22 12.9/27.0; Grok 2 12-12 25.7/14.5; Kimi K2 30.2/10.6; GPT-4o Mar 2025 38.1/3.5; Mistral Large 2 32.2/10.6; Qwen 2.5 Max 31.2/12.4; Mistral Medium 3 38.1/5.7; Llama 4 Maverick 28.2/16.9; Claude 3 Opus 28.2/17.2; Llama 3.3 70B 17.8/27.8; MiniMax-Text-01 44.6/3.3; Mistral Small 3 38.6/11.8; DeepSeek V3-0324 39.1/13.2; Gemini 2.0 Flash 24.3/29.4; Gemma 2 27B 47.0/7.2; GPT-4 Turbo 26.7/30.1; Microsoft Phi-4 52.5/6.4; Amazon Nova Pro 54.5/5.6; Claude 3 Haiku 56.9/11.5; Claude 3.5 Haiku 65.8/7.6; GPT-4o mini 60.9/13.5; Gemma 3 27B 66.3/14.2.

### LLM-AggreFact

[LLM-AggreFact](https://llm-aggrefact.github.io/) ranks fact-checkers, not summarisers; the page is undated.
- **Leaders:** nimble-v4-max 79.5, Clef 79.0, Decider v1.2 78.7, Jev 1.13.0 78.7, d1-ultra 78.5, nimble-v4 77.8, Bespoke-MiniCheck-7B 77.4 (CC BY-NC 4.0), Claude-3.5 Sonnet 77.2.
- **MiniCheck-Flan-T5-L:** 75.0 (0.8B).
- **HHEM-2.1-Open:** not on the page.

## Models built for summarisation

- **SummLlama** ([arXiv 2410.13116](https://arxiv.org/abs/2410.13116)): Llama 3 and 3.1 Instruct further trained with DPO on 100K+ items of summary feedback across seven domains (news, lifestyle, report, medical; daily life, interview, meeting).
  - Faithfulness / completeness / conciseness on FineSurE, from the [card](https://huggingface.co/DISLab/SummLlama3.1-8B): SummLlama3.1-8B 0.924 / 0.635 / 0.661, SummLlama3-70B 0.950 / 0.632 / 0.754, GPT-4o 0.940 / 0.657 / 0.437.
  - No inference provider on Hugging Face (2026-10-26), so self-host only.
  - It was trained on one template, "Please summarize the input documnet." (sic).
  - The card states no licence; it inherits Llama 3.1's.
- **BART-large-CNN** (MIT) and **PEGASUS-XSum**:
  - Live on Hugging Face's own `hf-inference` provider (2026-10-26), billed by compute time. Hugging Face says hf-inference "focuses mostly on CPU inference" ([pricing](https://huggingface.co/docs/inference-providers/pricing)).
  - Inputs are capped at 1,024 and 512 tokens (`max_position_embeddings`).
  - Trained on news summaries; they take no instructions.
  - The 16k-token LED and LongT5 summarisers have no provider.
- **LLMLingua-2** (Microsoft, MIT): keeps or drops each token; live on `hf-inference`.
- **Provence** (Naver, CC-BY-NC-ND-4.0): drops sentences; no provider.
- **Morph Compact** ([`morph-compactor`](https://www.morphllm.com/products/compact.md)): $0.20 / $0.50 per million, OpenAI-compatible. It deletes filler: "Every surviving sentence is byte-identical to the input". Retention isn't stated.
- **OpenAI `/responses/compact`:** returns an encrypted compaction item, "opaque and not intended to be human-interpretable" ([guide](https://developers.openai.com/api/docs/guides/compaction)).
- **Cohere `/v1/summarize`:** listed as deprecated in Cohere's [2025-09-15 changelog](https://docs.cohere.com/changelog/2025-09-15-major-command-deprecations).
- **"Lost in Context Compaction"** ([arXiv 2608.11242](https://arxiv.org/abs/2608.11242), v2 2026-10-11): compactors kept 17% of injected side constraints on average. After compaction, an agent deleted emails without approval in 30 of 100 sessions (none before), and a separate constraint extractor kept over 90%.

## Self-hosting on Modal

GPU prices per second ([pricing](https://modal.com/pricing), 2026-10-26):

| GPU | Per second | Per hour |
|---|---|---|
| L4 | $0.000222 | $0.80 |
| A10 | $0.000306 | |
| L40S | $0.000542 | |
| A100 80 GB | $0.000694 | $2.50 |
| H100 | $0.001097 | |

The Starter plan includes $30 of credit a month. How long a rebuild takes on any of these is unmeasured.

## Our evals

Method and every figure: `ai/evals/compactor/README.md`, results of 2026-10-26 and 27. Raw data: `.ai/local/evals/compactor/` (private). Each model built the whole summary tree of the same newest 256 notes under the 0.16.0 prompts (177 lines: 50 at level 1, 127 above). Each line was judged blind against Sonnet 5.5's line for the same node, each against its own two sources, by two judges in one random A/B order: Opus 5.5 (`claude -p`, high effort) and DeepSeek V4 Pro (thinking, high). Categories: unsupported claims, contradictions, meaning changed, dropped negations or user rules, facts lost.

Lines with a finding, run vs Sonnet on the same nodes, Opus judge / V4 Pro judge:

| Run | Nodes | Unsupported | Contradictions | Meaning changed | Any of the first four | Judged better |
|-|-|-|-|-|-|-|
| Flash, thinking off (10-26) | 177 | 37 vs 6 / 40 vs 18 | 22 vs 5 / 11 vs 6 | 140 vs 74 / 92 vs 54 | 160 vs 115 / 140 vs 122 | 45 vs 130 / 65 vs 110 |
| Opus 5.5, effort medium (10-26) | 177 | 11 vs 9 / 18 vs 13 | 0 vs 7 / 7 vs 5 | 96 vs 79 / 78 vs 52 | 131 vs 121 / 134 vs 128 | 120 vs 54 / 87 vs 88 |
| Flash, effort medium (10-27) | 172 | 9 vs 8 / 15 vs 7 | 12 vs 5 / 5 vs 5 | 115 vs 79 / 71 vs 61 | 134 vs 121 / 123 vs 121 | 75 vs 95 / 91 vs 79 |
| Flash, effort max (10-27) | 151 | 7 vs 7 / 22 vs 10 | 9 vs 6 / 5 vs 6 | 103 vs 58 / 50 vs 53 | 112 vs 91 / 88 vs 112 | 77 vs 71 / 92 vs 58 |
| Flash medium + Flash medium witness (10-27) | 32 | 1 vs 2 / – | 1 vs 1 / – | 20 vs 11 / – | 24 vs 12 / – | 12 vs 18 / – |

- **Format:** every run's final lines are within 512 characters, end in punctuation and leak no filler. Length retries: Sonnet 22, Opus 2, Flash thinking off 143, medium 10, max 29.
- **Hard facts kept per level** (`keep.py`): within a few points across all runs, 84–87% at level 1 and 35–40% at level 2.
- **Partial runs:** the Flash max tree's top line is unbuilt; neither judge finished the 10-27 runs (V4 Pro stopped on DeepSeek's concurrency cut, Opus on the Claude spend limit).

### Thinking off and on

- The 10-26 Flash run was the only one without thinking: `backend = "api"` then sent DeepSeek thinking disabled, while `claude -p --effort medium` gave Sonnet 12,791 thinking tokens and Opus 159,921. That comparison is not like for like.
- With thinking at medium, Flash's lines with unsupported claims fell from 37 to 9 (Opus judge) and from 40 to 15 (V4 Pro), contradictions from 22 to 12 and from 11 to 5, and length retries from 143 to 10.

### Judges

- **Self-preference:** on Sonnet vs Opus, the Opus judge preferred Opus 120 to 54; V4 Pro, on the same nodes in the same order, 87 to 88. In every comparison one judge shares a maker with one side.
- **Agreement** on the better line: 75% (Flash thinking off), 73% (Opus), 62% (Flash medium), 77% (Flash max).
- **Flash with thinking:** V4 Pro prefers Flash (91 to 79 at medium, 92 to 58 at max); the Opus judge prefers Sonnet at medium (95 to 75) and Flash at max (77 to 71).
- **Position:** V4 Pro preferred the line shown first in every comparison (101–74, 96–79, 99–71, 92–58); the Opus judge leaned either way (100–75, 83–91, 82–88, 78–70, 16–14). The order is random per node, so it favours neither run.
- **Read by hand,** independent of the judges: Flash with thinking off reversed a user decision (claude-in-chrome removed became kept and listed among the user's rules), named a project no note names ("deepseek-rs") and wrote "memlab" for "molab".

### Witness pass

`[summariser.witness]` (branch `feat/summariser-witness`) has a second model check every summary for unsupported claims, contradictions, changed meaning and dropped negations or rules; the summariser rewrites or contests, back and forth until the witness finds nothing, with no round limit. With Flash medium as both: all 177 lines resolved, 85 with no finding, 72 after one round, 13 after two, 3 after three, 3 after four, 1 after six; 193 findings, 3 contested; 1,280 s against 447 s without. Only 32 of its nodes were judged, by Opus alone; its effect on faithfulness is unmeasured.

### Cost

- With thinking on, thinking tokens are 96–99% of Flash's output: 665k output tokens at medium and 2.0M at max per tree, against 44k with thinking off.
- Per 177-line tree at list price: Flash $0.04 thinking off, $0.83 medium, $2.43 max, $2.65 with the witness; Sonnet $1.01 and Opus $4.81 at API price, billed to the subscription.
- V4 Pro judging, thinking at high: $0.018–0.038 per verdict, $18.89 over 680 verdicts, three quarters of the about $25 DeepSeek spend. Opus judging: about $7 per 177 verdicts at API price, on the subscription.
- DeepSeek reports no cost; the figures are list price times recorded tokens (`prices.py`).

## Open questions

- Does the witness pass lower Flash's faults? 145 of its nodes are unjudged, and V4 Pro judged none.
- A judge from neither maker (OpenAI, Google), so that no judge shares a maker with either side.
- The Flash max tree's top line and the unjudged nodes of the 10-27 runs.
- Sonnet and Opus at efforts other than medium; a witness at another effort or from another model.
- Thread summaries: only Sonnet has written them.
- Which categories the judges disagree on (62–77% agreement), read against the spot-check files.
- The free checks of the earlier plan (HHEM-2.1-Open, MiniCheck), and the survey's other models, are untried.

## Facts on "at or near Sonnet 5.5"

- **No public measurement:** no public summarisation or grounding board lists Sonnet 5.5. Measurements against Sonnet 5.5 on our task are our own: Haiku 5.5 fell short on 2026-10-08; DeepSeek Flash and Opus 5.5 are in [Our evals](#our-evals).
- **AA-Omniscience** (closed-book knowledge) lists Sonnet 5.5 at 47.0%. Models with lower rates there:
  - Gemini 4 Argon 15.1%, Muse Wonder Full 23.0%, Muse Spark Open 24.1%, K2 Horizon 25.9%, Muse Wonder 26.5%, Qwen3.8 Max 28.8%;
  - Grok 4.7 29.3%, GLM-5.3 29.6%, Qwen3.8 27B 30.3%, Nemotron 3.5 Super VL 35.8%, GPT-6.1 Terra 38.0%;
  - Haiku 5.5 40.4%, MiMo-V2.6-Pro 40.6%, Mistral Large 4 41.9%, Step 5 43.0%, Kimi K3.1 Pro 43.9%.

  Haiku 5.5 is among them, although it failed our eval.
- **Vectara** has no Sonnet 5.5 row. Opus 5.5 is at 9.9%, and Anthropic's other listed models run from 9.8% to 12.2%.
  - **At or below 9.9%, with a price found:** 48 models:
    - gpt-5.4-nano, Gemini 2.5 Flash-Lite, Phi-4, Llama 3.3 70B, DeepSeek V4.1 Flash, Gemma 3 12B, Nova Pro, Nova 2 Lite, Mistral Small 2501, Gemma 4 26B A4B, Jamba Mini 2, DeepSeek V3.2-Exp;
    - Qwen3 14B, Nova Micro, DeepSeek V3.1, gpt-5.4-mini, gpt-4.1, Qwen3 32B, Nova Lite, DeepSeek V3, DeepSeek V3.2, Gemma 3 4B, gpt-6-sol, Command R+ 08-2024;
    - gpt-5.4, Gemini 2.5 Pro, Gemma 3 27B, Gemma 4 31B, Llama 4 Scout, Gemini 2.5 Flash, Llama 4 Maverick, Gemini 3.1 Flash-Lite, gpt-5.4-pro, gpt-5.2 (low), DeepSeek V4 Pro, gpt-6-astra;
    - MiniMax M2.5, Command A, gpt-5.5, GLM-4.5-Air, Qwen3 Next 80B Thinking, GLM-4.7 Flash, GLM-4.6, Nemotron 3 Nano, gpt-4o, Jamba Large 1.7, Haiku 4.5, Opus 5.5.
  - **At or below 9.9%, but unavailable, retired or unpriced:** finix_s1_32b, Arctic Instruct, Mistral Large 2411, Qwen3 8B, Granite 4.0 H Small, Qwen3 4B, Grok 3, Trinity Large Preview, Ministral 3B and 8B 2410, Qwen3 235B A22B (2504), Aya Expanse 8B.
- **FACTS Grounding** has no Sonnet 5.5 row. It lists Haiku 5.5 at 65.8 and Opus 5.5 at 68.5. At or above 68.5:
  - gpt-6-sol 85.2, gpt-6.1-sol 82.4, gpt-6-astra 82.0, Gemma 4 26B A4B 80.9, Gemma 4 31B 80.7, Gemini 4 Argon 78.6, gpt-5.2 76.2, gpt-5.5 76.2;
  - Gemini 3.7 Flash 75.5, Gemini 2.5 Pro 74.3, Gemini 3.5 Flash 73.2, Gemini 3.8 Flash 73.1, Gemini 3.6 Flash 72.5, gpt-5.6-terra 71.2, Gemini 3.5 Flash-Lite 71.2;
  - gpt-5.4 70.3, Gemini 2.5 Flash 70.0, Opus 4.8 69.7, gpt-5 69.6, gpt-5.6-sol 68.7, Opus 5.5 68.5.
- **On both boards** at or better than Opus 5.5 (Vectara ≤ 9.9% and FACTS ≥ 68.5):
  - gpt-6-sol (6.5%, 85.2), Gemma 4 26B A4B (5.2%, 80.9), Gemma 4 31B (7.4%, 80.7);
  - gpt-5.4 (7.0%, 70.3), Gemini 2.5 Pro (7.0%, 74.3), Gemini 2.5 Flash (7.8%, 70.0);
  - gpt-5.2 low (8.4%, 76.2), gpt-6-astra (8.7%, 82.0), gpt-5.5 (9.3%, 76.2), Opus 5.5 (9.9%, 68.5).
