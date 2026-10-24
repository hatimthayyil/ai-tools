# Summariser models and providers

Author: Claude Code (Claude Opus 5.5)

How `ai` could let users choose the model and the provider for every summary it writes (chat, memory, files, notes). Today all of them go through `claude -p` (`src/memory/claude.rs`), behind the `Backend`/`Conversation` traits in `src/memory/backend.rs`. Each build is one conversation of several calls, and it relies on the prompt cache across those calls. Each call records its tokens, cache reads and writes, and cost.

## Verdict

Yes, libraries manage the providers. In Rust, `genai` fits the summariser best. Through a harness, `pi` covers the most providers. But no library may use a Claude subscription: only `claude -p` can, so the subscription route and the API route stay separate.

Recommended: two kinds of backend behind the existing trait.
1. **Harness, for subscription logins:** `claude -p`, as today. It covers individual plans (Pro, Max) and org plans (Team, Enterprise), and also API keys and cloud accounts. Add `codex exec` or `pi -p` only when someone needs another provider's subscription.
2. **API, for API keys:** one in-process backend on `genai`, which covers 27+ providers and any OpenAI-compatible endpoint. Models are named by their models.dev id, `provider/model`, and models.dev also supplies the prices behind `Usage::cost`.

**Main trade-off:** `genai` brings tokio, reqwest and rustls into a program that has no async runtime today. That adds about 175 crates, taking the lockfile from 203 to 378 packages. `ai` would also own the cache breakpoints and the price lookup. In return, users need no Node or Python runtime and no extra process per call. The alternative is the pi route, already built in `8307761c`. It adds no Rust crates, and pi handles caching, cost and the model list. But it needs Node 22 and pi installed, it starts a process per call, and pi's own Claude login breaks Anthropic's terms.

## Claude subscriptions outside Claude Code

Anthropic's [legal and compliance page](https://code.claude.com/docs/en/legal-and-compliance) says:
- OAuth "is intended exclusively for purchasers of Claude Free, Pro, Max, Team, and Enterprise subscription plans and is designed to support ordinary use of Claude Code and other native Anthropic applications."
- Developers, including those using the Agent SDK, "should use API key authentication". Anthropic does not permit them "to route requests through Free, Pro, or Max plan credentials on behalf of their users", nor to "collect, store, or intermediate Claude.ai credentials or session tokens".
- The page also says the policy does not prevent "an end user from signing in to the unmodified Claude Code binary with their own Claude subscription".
- "Advertised usage limits for Pro and Max plans assume ordinary, individual usage of Claude Code and the Agent SDK."
- The Consumer Terms (§3.7, [per The Register](https://www.theregister.com/software/2026/02/20/anthropic-clarifies-ban-on-third-party-tool-access-to-claude/5014546)) bar automated access except through an API key or where Anthropic explicitly permits it.

For `ai`:
- `claude -p` signed in with the user's own subscription is the unmodified binary, run in its documented headless mode. That is the only route these terms leave open for a subscription. The page doesn't say whether a third-party tool scripting `claude -p` counts as "ordinary, individual usage".
- Any library, proxy or other harness that uses a Claude subscription token is prohibited. That includes pi's `/login anthropic` (Claude Pro/Max OAuth, `packages/ai/src/auth/oauth/anthropic.ts`) and rig's `rig-claude-cloak` plugin.
- OpenAI tolerates ChatGPT logins in third-party harnesses such as pi and OpenCode, by public staff statements that aren't contractual ([summary](https://zenn.dev/ryoushin/articles/addd79c36e4e19)). Codex's own docs say "API keys are the right default for automation" ([non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)).

## Rust crates

Package counts are from `cargo generate-lockfile`: ai's dependencies at `ce319364`, plus the crate with default features. Lockfiles list every platform's packages, so the counts overstate what one build compiles.

| Crate | Providers | Model named as | Auth | Prompt cache | Releases | Licence | Lockfile |
|---|---|---|---|---|---|---|---|
| [`genai`](https://github.com/jeremychone/rust-genai) 0.7.1 | 27+ native: Anthropic, OpenAI (+Responses), Gemini, Vertex, Bedrock, DeepSeek, Mistral, xAI, Groq, Ollama, OpenRouter, Together, Fireworks, Cohere, Moonshot/Kimi, Z.ai, Qwen, Copilot…; custom OpenAI-compatible `genai_n::` | bare name inferred (`claude-…`, `gpt-…`), or `namespace::model` | API key per provider env var (`ANTHROPIC_API_KEY`…); `AuthResolver` for anything else | `CacheControl` per message or request (`Ephemeral5m`/`1h`/`24h`); usage reports cached and cache-creation tokens by TTL; **no cost** | 0.7.0 2026-10-20 after 9 RCs, 0.7.1 10-23; 0.6.0 2026-05-23; breaking at minor versions; 1 main author (957 commits, next 32); 916 stars | MIT OR Apache-2.0 | 378 (+175), tokio, reqwest, rustls, aws-lc-sys |
| [`rig-core`](https://github.com/0xPlaygrounds/rig) 0.45.0 | 20+: Anthropic, OpenAI, ChatGPT plan, Copilot, Gemini, Azure, DeepSeek, Mistral, xAI, Groq, Ollama, llama.cpp, OpenRouter, Together, Moonshot, Z.ai, MiniMax… | `vendor/model` via `Catalog::connect`, catalogue generated from models.dev | API key from env; `ProviderConfig` | Anthropic `cache_control`; catalogue holds cache facts and **prices** | a breaking minor every 1–6 weeks (0.37 May → 0.45 Oct, 0.46-rc out); several active authors; 9.0k stars; now also an agent framework | MIT | 371 (+168), tokio, reqwest |
| [`llm`](https://github.com/graniet/llm) 1.3.8 | 13: OpenAI, Anthropic, Ollama, DeepSeek, xAI, Groq, Gemini, Cohere, Mistral, OpenRouter… | builder `.backend(LLMBackend::X).model(..)`; CLI `provider:model` | API key | none documented | last release 2026-04-19, last push 2026-06-06 | MIT | 551 (+348) |
| [`embacle`](https://github.com/dravr-ai/dravr-embacle) 0.39.1 | 12 CLI harnesses (Claude Code, Codex, Gemini CLI, OpenCode…); HTTP APIs behind `http-api` | `provider:model` | each CLI's own login; API keys | Anthropic breakpoints over HTTP; **CLI runners are stateless and never resume**, so no cache across a build's calls | every few days since 2026-03; 1 author; 8 stars | Apache-2.0 | 214 (+11), tokio |

Others found are SDKs for one provider (`async-openai`, `ollama-rs`, `misanthropic`) or frameworks built on these (`langchain-rust`, `swiftide`, `adk-rust`).

## Without a library

| Route | Providers | Model named as | Auth | Prompt cache | Notes |
|---|---|---|---|---|---|
| `claude -p --model` (today) | Claude only: Anthropic API, Bedrock, Google Agent Platform, Foundry; gateways through `ANTHROPIC_BASE_URL`, but Anthropic "doesn't support routing Claude Code to non-Claude models" | aliases `sonnet`, `opus`, `haiku`, `fable`, or a full name (ARN or deployment name on cloud providers) | subscription (individual and org), API key, cloud credentials | Claude Code's own marks; `ai` keeps one session per build, 5-minute TTL | reports cost; proprietary binary |
| `pi -p --model provider/model` | 30+ through pi-ai (`docs/providers.md`) | `provider/model` | `/login` OAuth (Claude Pro/Max — prohibited, see above; ChatGPT/Codex, Copilot, Kimi, xAI, OpenRouter) or API key env vars; `auth.json` key may be a `!command` | automatic Anthropic `cache_control`; `PI_CACHE_RETENTION=long`; cache warming | reports cost; Node ≥ 22.19; MIT; active (last commit 2026-10-22); a process per call |
| `codex exec -m` | OpenAI; `--oss` (Ollama, LM Studio); others via `-c model_provider` or profiles | OpenAI model name | ChatGPT login or `CODEX_API_KEY` | OpenAI's automatic caching; `--json` reports `cached_input_tokens`; `codex exec resume <id>` continues a session | brings Codex's agent prompt and tools (sandbox `read-only`); not a plain completion |
| [LiteLLM](https://github.com/BerriAI/litellm) | 100+ | `provider/model` | keys held by the proxy | passes `cache_control` through | a Python service to run; 62k stars, 5.6k open issues; PyPI releases 1.82.7 and 1.82.8 were backdoored on 2026-03-24 ([advisory](https://docs.litellm.ai/blog/security-update-march-2026)); fits only as an endpoint a user supplies |
| [OpenRouter](https://openrouter.ai/docs/features/prompt-caching) | hosted router over most providers | `vendor/model` | one OpenRouter key; BYOK | automatic for OpenAI, DeepSeek, Gemini 2.5+, Grok; Anthropic needs `cache_control`; sticky routing for 10 min keeps hits | a provider, not a library: `genai`, rig and pi all reach it; about 5.5% fee on credit purchases (third-party sources) |
| [models.dev](https://github.com/anomalyco/models.dev) | catalogue, not a client | `provider/model` | per provider: `env` key names, `api` base URL | per model: `cost.cache_read`, `cost.cache_write` (USD per M tokens) | `https://models.dev/api.json`; MIT; 7.4k stars, active; used by OpenCode and rig's catalogue |

## Why `genai` over the others

- **rig-core:** has a price catalogue, but it breaks its API every few weeks. It has grown into an agent framework, and it ships a Claude subscription plugin, which Anthropic's terms prohibit.
- **llm:** has stalled, is the heaviest, and documents no prompt caching.
- **embacle:** has one author, and its CLI runners never resume a session, so a build's calls can't reuse the cache.
- **A hand-written client:** `ureq` on Anthropic Messages and OpenAI Chat Completions would reach most providers. It adds 68 crates and stays synchronous. It is only the fallback if tokio is unacceptable, because the project's rule is to use established libraries first.
- **What genai lacks:** cost reporting. models.dev prices fill that gap: input, output and both cache rates per model.

## The DeepSeek Harness change, for comparison

- **`8307761c` (pi backend):** `pi -p --mode json --session-id <id>`, continuing one session per conversation, with no tools, extensions, skills, context files, prompt templates or MCP, and thinking off. The prompt goes on stdin, and the last `message_end` gives the text, usage and cost.
- **`115c6aa0` (config):** `[summariser] backend = "claude" | "pi"`, and `model` in each backend's own spelling (`sonnet` or `provider/model`). `$AI_MEMORY_MODEL` overrides both, and pi has no default.

Against the recommendation:
- **Per-backend model spellings:** the same Claude model is written differently for each backend. models.dev ids would give one name space, at least for the API backend.
- **`$AI_MEMORY_MODEL`:** a second route beside `ai.toml`.
- **The pi backend** is the one place where a user could sign pi into a Claude subscription. If kept, it should refuse `anthropic` with OAuth credentials.
