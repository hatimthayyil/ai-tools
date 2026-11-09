use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use genai::{
    Client, ModelIden, ServiceTarget,
    adapter::AdapterKind,
    chat::{
        ChatMessage, ChatOptions, ChatRequest, ChatResponse, ContentPart, MessageContent,
        ReasoningEffort, StopReason,
    },
    resolver::AuthData,
};
use serde_json::{Value, json};
use tokio::{runtime::Runtime, sync::watch};

use crate::{
    command,
    memory::{
        backend::{Backend, Block, Conversation, Usage},
        claude::{CALL_TIMEOUT, Record, tail, user_message, uuid},
    },
};

/// A model behind a provider's API, through `genai`. Its `model` is `<provider>/<model>`
/// ([`Provider`]), whose key the provider's command in `[summariser.keys]` prints, else is read
/// from that adapter's environment variable (`DEEPSEEK_API_KEY`, …). Its `effort` goes to the
/// provider as genai maps it; unset, none goes, and the provider's own default holds. Each
/// conversation keeps its [`Record`] in `calls`; the key goes only in the requests' headers,
/// which no record keeps.
pub struct Api {
    shared: Arc<Shared>,
}

/// What every conversation of one [`Api`] uses.
struct Shared {
    provider: String,
    model: String,
    effort: Option<ReasoningEffort>,
    jobs: usize,
    runtime: Runtime,
    client: Client,
    target: ServiceTarget,
    options: ChatOptions,
    /// True once [`Backend::stop`] ran: calls in flight fail, and no conversation opens.
    stop: watch::Sender<bool>,
    calls: PathBuf,
}

/// The provider a `[summariser] model` names, one of genai's adapters (`deepseek`, `openai`,
/// `anthropic`, `gemini`, `open_router`, …), and the model as that provider spells it.
pub struct Provider<'m> {
    kind: AdapterKind,
    name: &'m str,
    model: &'m str,
}

impl<'m> Provider<'m> {
    /// The adapter `name` names, as the setting `within` gives it; an error if genai knows no
    /// such provider.
    pub fn named(name: &str, within: &str) -> Result<AdapterKind> {
        AdapterKind::from_lower_str(name).with_context(|| {
            format!(
                "`{name}` in `{within}` is no provider genai knows: use one such as deepseek, \
                 openai, anthropic, gemini or open_router."
            )
        })
    }

    /// The provider of `model`, `<provider>/<model>`.
    pub fn of(model: &'m str) -> Result<Provider<'m>> {
        let (name, spelt) = model
            .split_once('/')
            .filter(|(p, m)| !p.is_empty() && !m.is_empty())
            .with_context(|| {
                format!(
                    "`[summariser] model = \"{model}\"` names no provider: with `backend = \"api\"`, \
                     it is `<provider>/<model>`, such as \"deepseek/deepseek-flash\"."
                )
            })?;
        let kind = Provider::named(name, "[summariser] model")?;
        Ok(Provider {
            kind,
            name,
            model: spelt,
        })
    }

    /// The command among `keys` (`[summariser.keys]`) that prints this provider's key, and no
    /// other provider's; `None` when its environment variable holds the key instead. An error
    /// when neither gives one.
    pub fn key_command<'k>(&self, keys: &'k BTreeMap<String, String>) -> Result<Option<&'k str>> {
        let name = self.name;
        if let Some(command) = keys.get(name) {
            return Ok(Some(command));
        }
        if let Some(var) = self.kind.default_key_env_name()
            && std::env::var_os(var).is_none_or(|v| v.is_empty())
        {
            bail!(
                "Set {var} to your {name} API key, or name a command that prints it as \
                 `[summariser.keys] {name}` in your own ai.toml: the summariser calls the {name} \
                 API."
            );
        }
        Ok(None)
    }
}

impl Api {
    /// `model`, `<provider>/<model>`, thinking at `effort` (`None`: the provider's default),
    /// `jobs` calls at once in a build, its key printed by its provider's command among `keys`
    /// (none: the provider's environment variable), keeping its calls in `calls`; fails without
    /// the provider's key.
    pub fn new(
        model: &str,
        effort: Option<ReasoningEffort>,
        jobs: usize,
        keys: &BTreeMap<String, String>,
        calls: &Path,
    ) -> Result<Api> {
        let provider = Provider::of(model)?;
        let kind = provider.kind;
        let client = match provider.key_command(keys)? {
            Some(command) => {
                let key = key(provider.name, command)?;
                // The key is this provider's alone: a model of any other gets none of it.
                Client::builder()
                    .with_auth_resolver_fn(move |m: ModelIden| {
                        Ok((m.adapter_kind == kind).then(|| AuthData::from_single(key.clone())))
                    })
                    .build()?
            }
            None => Client::new()?,
        };
        let runtime = runtime()?;
        let target = runtime
            .block_on(client.resolve_service_target(ModelIden::new(kind, provider.model)))?;
        Ok(Api::at(
            provider.name,
            effort,
            jobs,
            runtime,
            client,
            target,
            calls,
        ))
    }

    fn at(
        provider: &str,
        effort: Option<ReasoningEffort>,
        jobs: usize,
        runtime: Runtime,
        client: Client,
        target: ServiceTarget,
        calls: &Path,
    ) -> Api {
        let (stop, _) = watch::channel(false);
        let mut options = ChatOptions::default().with_capture_raw_body(true);
        if let Some(e) = &effort {
            options = options.with_reasoning_effort(e.clone());
        }
        Api {
            shared: Arc::new(Shared {
                provider: provider.into(),
                model: target.model.model_name.to_string(),
                effort,
                jobs,
                runtime,
                client,
                target,
                options,
                stop,
                calls: calls.into(),
            }),
        }
    }
}

/// The keys the commands of this process printed, by command.
static KEYS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// The key `provider`'s `command` prints ([`printed`]), run once in this process however many
/// backends ask: each round of a build makes its own. A command that failed runs again.
fn key(provider: &str, command: &str) -> Result<String> {
    // Held while the command runs, so two backends starting at once run it once.
    let mut keys = KEYS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(key) = keys.get(command) {
        return Ok(key.clone());
    }
    let key = printed(provider, command, command::LIMIT)?;
    keys.insert(command.into(), key.clone());
    Ok(key)
}

/// The key `provider`'s `command` prints within `limit` ([`command::printed`]): its first line,
/// without the blanks around it, as `pass` keeps a secret. Its errors name the command, never
/// what it printed: that is the key.
fn printed(provider: &str, command: &str, limit: Duration) -> Result<String> {
    let setting = format!("[summariser.keys] {provider}");
    let text = command::printed(&setting, command, limit)?;
    match text.lines().next().unwrap_or_default().trim() {
        "" => bail!("`{setting} = {command:?}` printed no key on its first line."),
        key => Ok(key.to_string()),
    }
}

/// One runtime for every call of a backend: calls block on it from the summariser's threads.
fn runtime() -> Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("ai-api")
        .enable_all()
        .build()
        .context("Cannot start the API client's runtime.")
}

impl Backend for Api {
    fn agent(&self) -> &str {
        &self.shared.provider
    }

    fn model(&self) -> &str {
        &self.shared.model
    }

    fn jobs(&self) -> usize {
        self.shared.jobs
    }

    fn open(&self, system: &str) -> Result<Box<dyn Conversation>> {
        let s = &self.shared;
        if *s.stop.borrow() {
            bail!("{} owner has stopped", s.provider);
        }
        let id = uuid()?;
        let record = Record::start(
            &s.calls,
            &id,
            &json!({
                "type": "ai.call",
                "api": s.provider,
                "model": s.model,
                "endpoint": s.target.endpoint.base_url(),
                "reasoning_effort": s.effort.as_ref().map(ReasoningEffort::to_string),
                "system": system,
            }),
        )?;
        Ok(Box::new(Chat {
            shared: self.shared.clone(),
            request: ChatRequest::from_system(system),
            id,
            record,
            used: Usage::default(),
        }))
    }

    fn stop(&self) {
        self.shared.stop.send_replace(true);
    }
}

/// One conversation: the whole exchange so far goes with each request.
struct Chat {
    shared: Arc<Shared>,
    request: ChatRequest,
    id: String,
    record: Record,
    /// What the last reply took.
    used: Usage,
}

impl Chat {
    /// The response to `request`; an error when it fails, times out or the backend stops.
    fn call(&self, request: ChatRequest) -> Result<ChatResponse, String> {
        let s = &self.shared;
        let mut stop = s.stop.subscribe();
        let call = s
            .client
            .exec_chat(s.target.clone(), request, Some(&s.options));
        s.runtime.block_on(async {
            tokio::select! {
                r = tokio::time::timeout(CALL_TIMEOUT, call) => match r {
                    Ok(r) => r.map_err(|e| e.to_string()),
                    Err(_) => Err(format!("no reply after {} s.", CALL_TIMEOUT.as_secs())),
                },
                _ = stop.wait_for(|&stopped| stopped) => Err("stopped".into()),
            }
        })
    }
}

impl Conversation for Chat {
    fn say(&mut self, message: &[Block]) -> Result<String> {
        self.used = Usage::default();
        let provider = &self.shared.provider;
        self.record.sent(&user_message(message));
        let parts: Vec<ContentPart> = message
            .iter()
            .map(|b| ContentPart::Text(b.text.clone()))
            .collect();
        let request = self
            .request
            .clone()
            .append_message(ChatMessage::user(MessageContent::from_parts(parts)));
        let res = match self.call(request.clone()) {
            Ok(r) => r,
            Err(e) => {
                self.record.event(&json!({"type": "ai.error", "error": e}));
                self.record.check()?;
                bail!("{provider}: {}", tail(&e, 300));
            }
        };
        self.record.event(&json!({
            "type": "ai.reply",
            "usage": res.usage,
            "body": res.captured_raw_body,
        }));
        self.record.check()?;
        self.used = usage(&res);
        if matches!(res.stop_reason, Some(StopReason::ContentFilter(_))) {
            bail!("{provider} filtered the reply")
        }
        let text = res
            .first_text()
            .with_context(|| format!("{provider}: the reply has no text"))?
            .to_string();
        self.request = request.append_message(ChatMessage::assistant(text.clone()));
        Ok(text)
    }

    fn session(&self) -> String {
        self.id.clone()
    }

    fn used(&self) -> Usage {
        self.used
    }
}

/// What `res` took. Input excludes the tokens read from or written to the cache; DeepSeek
/// counts its cache reads as `prompt_cache_hit_tokens`. Thinking is the output's reasoning
/// tokens. The cost is the provider's own figure (`usage.cost`, as OpenRouter reports it), else
/// zero.
fn usage(res: &ChatResponse) -> Usage {
    let n = |v: Option<i32>| v.map_or(0, |v| v.max(0) as u64);
    let body = res.captured_raw_body.as_ref().unwrap_or(&Value::Null);
    let thinking = res.usage.completion_tokens_details.as_ref();
    let details = res.usage.prompt_tokens_details.as_ref();
    let cache_read = match n(details.and_then(|d| d.cached_tokens)) {
        0 => body["usage"]["prompt_cache_hit_tokens"]
            .as_u64()
            .unwrap_or(0),
        c => c,
    };
    let cache_write = n(details.and_then(|d| d.cache_creation_tokens));
    Usage {
        input: n(res.usage.prompt_tokens).saturating_sub(cache_read + cache_write),
        cache_write,
        cache_read,
        output: n(res.usage.completion_tokens),
        thinking: n(thinking.and_then(|d| d.reasoning_tokens)),
        cost: body["usage"]["cost"].as_f64().unwrap_or(0.0),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::{Mutex, mpsc},
        thread,
        time::Duration,
    };

    use genai::resolver::{AuthData, Endpoint};
    use tempfile::TempDir;

    use super::*;

    /// A server speaking OpenAI's chat completions on a Unix socket, so tests need no network.
    pub(crate) struct Server {
        dir: TempDir,
        /// Each request's body.
        pub(crate) sent: mpsc::Receiver<Value>,
        /// Each request's `authorization` header.
        auths: Arc<Mutex<Vec<String>>>,
    }

    /// Answers each request with the next of `replies` (status, body), the last one again once
    /// they run out; status 0 never answers.
    pub(crate) fn serve(replies: Vec<(u16, Value)>) -> Server {
        let dir = TempDir::new().unwrap();
        let listener = UnixListener::bind(dir.path().join("socket")).unwrap();
        let (tx, sent) = mpsc::channel();
        let replies = Arc::new(Mutex::new(replies));
        let auths = Arc::new(Mutex::new(Vec::new()));
        let heard = auths.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                let (tx, replies, heard) = (tx.clone(), replies.clone(), heard.clone());
                thread::spawn(move || answer(conn.unwrap(), &tx, &replies, &heard));
            }
        });
        Server { dir, sent, auths }
    }

    impl Server {
        /// DeepSeek's Flash through this server at its default effort, keeping its calls in
        /// `calls`.
        pub(crate) fn api(&self, calls: &Path) -> Api {
            self.api_at(None, calls)
        }

        /// DeepSeek's Flash through this server at `effort`, 3 calls at once, keeping its calls
        /// in `calls`.
        pub(crate) fn api_at(&self, effort: Option<ReasoningEffort>, calls: &Path) -> Api {
            self.keyed(effort, "test-key", calls)
        }

        /// [`Server::api_at`] with `key`.
        fn keyed(&self, effort: Option<ReasoningEffort>, key: &str, calls: &Path) -> Api {
            let http = reqwest::Client::builder()
                .unix_socket(self.dir.path().join("socket"))
                .build()
                .unwrap();
            let client = Client::builder().with_reqwest(http).build().unwrap();
            let target = ServiceTarget {
                endpoint: Endpoint::from_static("http://localhost/v1/"),
                auth: AuthData::from_single(key),
                model: ModelIden::new(AdapterKind::DeepSeek, "deepseek-flash"),
            };
            Api::at(
                "deepseek",
                effort,
                3,
                runtime().unwrap(),
                client,
                target,
                calls,
            )
        }
    }

    fn answer(
        conn: UnixStream,
        tx: &mpsc::Sender<Value>,
        replies: &Mutex<Vec<(u16, Value)>>,
        auths: &Mutex<Vec<String>>,
    ) {
        let mut out = conn.try_clone().unwrap();
        let mut conn = BufReader::new(conn);
        loop {
            let mut length = 0;
            loop {
                let mut line = String::new();
                if conn.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        length = v.trim().parse().unwrap();
                    } else if k.eq_ignore_ascii_case("authorization") {
                        auths.lock().unwrap().push(v.trim().to_string());
                    }
                }
            }
            let mut body = vec![0; length];
            conn.read_exact(&mut body).unwrap();
            let _ = tx.send(serde_json::from_slice(&body).unwrap());
            let (status, reply) = {
                let mut r = replies.lock().unwrap();
                match r.len() {
                    1 => r[0].clone(),
                    _ => r.remove(0),
                }
            };
            if status == 0 {
                thread::sleep(Duration::from_secs(60));
                return;
            }
            let reply = reply.to_string();
            write!(
                out,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    }

    pub(crate) fn completion(text: &str, usage: Value) -> Value {
        json!({
            "id": "c1",
            "object": "chat.completion",
            "model": "deepseek-v4.1-flash-1017",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop",
            }],
            "usage": usage,
        })
    }

    #[test]
    fn a_conversation_sends_the_exchange_so_far_at_the_provider_s_effort_and_keeps_it() {
        let usage = json!({
            "prompt_tokens": 100, "completion_tokens": 7, "total_tokens": 107,
            "prompt_cache_hit_tokens": 60, "prompt_cache_miss_tokens": 40,
        });
        let server = serve(vec![
            (200, completion("a long line", usage.clone())),
            (200, completion("short", usage)),
        ]);
        let tmp = TempDir::new().unwrap();
        let calls = tmp.path().join("calls");
        let api = server.api(&calls);
        assert_eq!(
            (api.agent(), api.model(), api.jobs()),
            ("deepseek", "deepseek-flash", 3)
        );
        let mut conv = api.open("Write a line.").unwrap();
        assert_eq!(conv.say(&[Block::new("first")]).unwrap(), "a long line");
        let u = conv.used();
        assert_eq!(
            (
                u.input,
                u.cache_read,
                u.cache_write,
                u.output,
                u.thinking,
                u.cost
            ),
            (40, 60, 0, 7, 0, 0.0)
        );
        assert_eq!(conv.say(&[Block::new("shorter")]).unwrap(), "short");
        let one = server.sent.recv().unwrap();
        assert_eq!(one["model"], "deepseek-flash");
        assert!(one.get("thinking").is_none(), "{one}");
        assert!(one.get("reasoning_effort").is_none(), "{one}");
        let two = server.sent.recv().unwrap();
        let said: Vec<(&str, &str)> = two["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["role"].as_str().unwrap(), text(&m["content"])))
            .collect();
        assert_eq!(
            said,
            [
                ("system", "Write a line."),
                ("user", "first"),
                ("assistant", "a long line"),
                ("user", "shorter"),
            ]
        );
        let id = conv.session();
        drop(conv);
        let record = fs::read_to_string(calls.join(format!("{id}.jsonl"))).unwrap();
        let lines: Vec<Value> = record
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            ["ai.call", "ai.sent", "ai.reply", "ai.sent", "ai.reply"]
        );
        assert_eq!(lines[0]["api"], "deepseek");
        assert_eq!(lines[0]["model"], "deepseek-flash");
        assert_eq!(lines[0]["system"], "Write a line.");
        assert_eq!(lines[0]["reasoning_effort"], Value::Null);
        assert_eq!(
            lines[1]["line"],
            serde_json::from_str::<Value>(&user_message(&[Block::new("first")])).unwrap()
        );
        assert_eq!(lines[2]["body"]["model"], "deepseek-v4.1-flash-1017");
        assert_eq!(lines[2]["body"]["usage"]["prompt_cache_hit_tokens"], 60);
        assert_eq!(
            lines[4]["body"]["choices"][0]["message"]["content"],
            "short"
        );
    }

    /// A message's text, whether sent as a string or as parts.
    pub(crate) fn text(content: &Value) -> &str {
        content
            .as_str()
            .or_else(|| content[0]["text"].as_str())
            .unwrap()
    }

    #[test]
    fn an_effort_turns_deepseek_s_thinking_on_or_off_and_its_tokens_are_counted() {
        let usage = json!({
            "prompt_tokens": 30, "completion_tokens": 50,
            "completion_tokens_details": {"reasoning_tokens": 42},
        });
        for (effort, thinking, sent) in [
            (ReasoningEffort::High, "enabled", Some("high")),
            (ReasoningEffort::Max, "enabled", Some("max")),
            (ReasoningEffort::Zero, "disabled", None),
        ] {
            let server = serve(vec![(200, completion("ok", usage.clone()))]);
            let tmp = TempDir::new().unwrap();
            let mut conv = server
                .api_at(Some(effort.clone()), tmp.path())
                .open("s")
                .unwrap();
            conv.say(&[Block::new("m")]).unwrap();
            let u = conv.used();
            assert_eq!((u.output, u.thinking), (50, 42), "{effort}");
            let body = server.sent.recv().unwrap();
            assert_eq!(body["thinking"], json!({"type": thinking}), "{effort}");
            assert_eq!(body["reasoning_effort"].as_str(), sent, "{effort}");
            let record = fs::read_to_string(tmp.path().join(format!("{}.jsonl", conv.session())));
            let first: Value =
                serde_json::from_str(record.unwrap().lines().next().unwrap()).unwrap();
            assert_eq!(first["reasoning_effort"], effort.to_string());
        }
    }

    #[test]
    fn a_provider_s_own_cost_and_cache_counts_are_kept() {
        let usage = json!({
            "prompt_tokens": 50, "completion_tokens": 5,
            "prompt_tokens_details": {"cached_tokens": 20}, "cost": 0.0021,
        });
        let server = serve(vec![(200, completion("ok", usage))]);
        let tmp = TempDir::new().unwrap();
        let mut conv = server.api(tmp.path()).open("s").unwrap();
        conv.say(&[Block::new("m")]).unwrap();
        let u = conv.used();
        assert_eq!((u.input, u.cache_read, u.output), (30, 20, 5));
        assert!((u.cost - 0.0021).abs() < 1e-12, "{u:?}");
    }

    #[test]
    fn a_failed_call_is_an_error_naming_the_provider_and_is_kept() {
        let server = serve(vec![(
            429,
            json!({"error": {"message": "Rate limit reached", "type": "rate_limit"}}),
        )]);
        let tmp = TempDir::new().unwrap();
        let mut conv = server.api(tmp.path()).open("s").unwrap();
        let err = conv.say(&[Block::new("m")]).unwrap_err().to_string();
        assert!(err.starts_with("deepseek: "), "{err}");
        assert!(err.contains("Rate limit reached"), "{err}");
        assert_eq!(conv.used(), Usage::default());
        let record = fs::read_to_string(tmp.path().join(format!("{}.jsonl", conv.session())));
        let last: Value = serde_json::from_str(record.unwrap().lines().last().unwrap()).unwrap();
        assert_eq!(last["type"], "ai.error");
        assert!(last["error"].as_str().unwrap().contains("429"), "{last}");
    }

    #[test]
    fn a_filtered_or_empty_reply_is_an_error() {
        let mut filtered = completion("", json!({"prompt_tokens": 1, "completion_tokens": 0}));
        filtered["choices"][0]["finish_reason"] = json!("content_filter");
        let server = serve(vec![(200, filtered)]);
        let tmp = TempDir::new().unwrap();
        let mut conv = server.api(tmp.path()).open("s").unwrap();
        let err = conv.say(&[Block::new("m")]).unwrap_err().to_string();
        assert_eq!(err, "deepseek filtered the reply");

        let mut empty = completion("", json!({"prompt_tokens": 1, "completion_tokens": 0}));
        empty["choices"][0]["message"]["content"] = Value::Null;
        let server = serve(vec![(200, empty)]);
        let mut conv = server.api(tmp.path()).open("s").unwrap();
        let err = conv.say(&[Block::new("m")]).unwrap_err().to_string();
        assert_eq!(err, "deepseek: the reply has no text");
    }

    #[test]
    fn stopping_ends_calls_in_flight_and_refuses_new_conversations() {
        let server = serve(vec![(0, Value::Null)]);
        let tmp = TempDir::new().unwrap();
        let api = Arc::new(server.api(tmp.path()));
        let mut conv = api.open("s").unwrap();
        let caller = thread::spawn(move || conv.say(&[Block::new("m")]));
        server.sent.recv_timeout(Duration::from_secs(5)).unwrap();
        api.stop();
        let err = caller.join().unwrap().unwrap_err().to_string();
        assert_eq!(err, "deepseek: stopped");
        assert!(api.open("s").is_err());
    }

    /// `[summariser.keys]` with `command` for `provider`.
    fn keys(provider: &str, command: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(provider.to_string(), command.to_string())])
    }

    #[test]
    fn a_key_is_the_first_line_its_command_prints() {
        for (command, key) in [
            ("printf '  k-1 \\n'", "k-1"),
            ("echo noise >&2; printf k-2", "k-2"),
            ("printf 'k-3\\nlogin: me\\nurl: example.com\\n'", "k-3"),
        ] {
            assert_eq!(printed("heabsy", command, command::LIMIT).unwrap(), key);
        }
    }

    #[test]
    fn a_key_s_command_that_fails_or_prints_no_key_is_an_error_never_showing_its_output() {
        for (command, says) in [
            (
                "false",
                "`[summariser.keys] heabsy = \"false\"` failed: exit status: 1.",
            ),
            ("printf 's%s' 3cret; exit 3", "failed: exit status: 3."),
            ("printf 's%s' 3cret; kill -9 $$", "failed: signal: 9"),
            (
                "true",
                "`[summariser.keys] heabsy = \"true\"` printed no key on its first line.",
            ),
            ("printf '  \\n'", "printed no key on its first line."),
            (
                "printf '\\ns%s\\n' 3cret",
                "printed no key on its first line.",
            ),
            (
                "printf 's%s\\377' 3cret",
                "printed something that is not text",
            ),
        ] {
            let err = printed("heabsy", command, command::LIMIT)
                .unwrap_err()
                .to_string();
            assert!(err.contains(says), "{command}: {err}");
            assert!(!err.contains("s3cret"), "{command}: {err}");
        }
    }

    #[test]
    fn a_key_s_command_that_hangs_is_stopped() {
        let began = std::time::Instant::now();
        let err = printed("heabsy", "sleep 30", Duration::from_millis(200))
            .unwrap_err()
            .to_string();
        assert!(began.elapsed() < Duration::from_secs(5));
        assert!(
            err.contains("`[summariser.keys] heabsy = \"sleep 30\"` did not end within"),
            "{err}"
        );
    }

    #[test]
    fn a_key_s_command_runs_once_however_many_backends_start() {
        let tmp = TempDir::new().unwrap();
        let runs = tmp.path().join("runs");
        let command = format!("echo run >> '{}'; printf k-once", runs.display());
        for _ in 0..3 {
            let api = Api::new(
                "heabsy/qwen38",
                None,
                1,
                &keys("heabsy", &command),
                tmp.path(),
            )
            .unwrap();
            assert_eq!(api.shared.target.auth.single_key_value().unwrap(), "k-once");
        }
        assert_eq!(fs::read_to_string(&runs).unwrap(), "run\n");

        let failing = format!("echo run >> '{}'; exit 2", runs.display());
        for _ in 0..2 {
            let err = Api::new(
                "heabsy/qwen38",
                None,
                1,
                &keys("heabsy", &failing),
                tmp.path(),
            )
            .err()
            .unwrap()
            .to_string();
            assert!(err.contains("failed: exit status: 2."), "{err}");
        }
        assert_eq!(fs::read_to_string(&runs).unwrap(), "run\nrun\nrun\n");
    }

    #[test]
    fn a_key_goes_to_the_provider_it_is_set_for_and_no_other() {
        let tmp = TempDir::new().unwrap();
        let ran = tmp.path().join("ran");
        let command = format!("touch '{}'; printf k-deepseek", ran.display());
        let set = keys("deepseek", &command);
        if std::env::var_os("HEABSY_API_KEY").is_none() {
            let err = Api::new("heabsy/qwen38", None, 1, &set, tmp.path())
                .err()
                .unwrap()
                .to_string();
            assert!(
                err.contains("Set HEABSY_API_KEY to your heabsy API key"),
                "{err}"
            );
            assert!(err.contains("`[summariser.keys] heabsy`"), "{err}");
            assert!(!ran.exists(), "another provider's command ran");
        }

        let api = Api::new("deepseek/deepseek-flash", None, 1, &set, tmp.path()).unwrap();
        assert_eq!(
            api.shared.target.auth.single_key_value().unwrap(),
            "k-deepseek"
        );
        let s = &api.shared;
        let other = ModelIden::new(AdapterKind::OpenAI, "gpt-5");
        let auth = s
            .runtime
            .block_on(s.client.resolve_service_target(other))
            .map(|t| t.auth.single_key_value().ok());
        assert_ne!(
            auth.ok().flatten().as_deref(),
            Some("k-deepseek"),
            "the client gave deepseek's key to another provider"
        );
    }

    #[test]
    fn a_key_s_command_gives_the_provider_s_key_whatever_the_environment_holds() {
        let tmp = TempDir::new().unwrap();
        let api = Api::new(
            "heabsy/qwen38",
            None,
            1,
            &keys("heabsy", "printf 'k-from-command\\n'"),
            tmp.path(),
        )
        .unwrap();
        assert_eq!(
            api.shared.target.auth.single_key_value().unwrap(),
            "k-from-command"
        );
    }

    #[test]
    fn the_key_goes_in_each_request_s_header_and_in_no_record() {
        let server = serve(vec![
            (
                401,
                json!({"error": {"message": "Authentication Fails", "type": "authentication_error"}}),
            ),
            (
                200,
                completion("fine", json!({"prompt_tokens": 1, "completion_tokens": 1})),
            ),
        ]);
        let tmp = TempDir::new().unwrap();
        let api = server.keyed(None, "k-9f3a-secret", tmp.path());
        let mut conv = api.open("s").unwrap();
        let err = conv.say(&[Block::new("m")]).unwrap_err().to_string();
        assert!(!err.contains("k-9f3a"), "{err}");
        conv.say(&[Block::new("m")]).unwrap();
        assert_eq!(*server.auths.lock().unwrap(), ["Bearer k-9f3a-secret"; 2]);
        let record =
            fs::read_to_string(tmp.path().join(format!("{}.jsonl", conv.session()))).unwrap();
        assert!(record.contains("Authentication Fails"), "{record}");
        assert!(!record.contains("k-9f3a"), "{record}");
    }

    #[test]
    fn a_model_must_name_a_known_provider() {
        let tmp = TempDir::new().unwrap();
        for (model, says) in [
            ("sonnet", "names no provider"),
            ("deepseek/", "names no provider"),
            ("/deepseek-flash", "names no provider"),
            (
                "nosuch/model",
                "`nosuch` in `[summariser] model` is no provider genai knows",
            ),
            ("heabsy/qwen38", "Set HEABSY_API_KEY to your heabsy API key"),
        ] {
            if model.starts_with("heabsy") && std::env::var_os("HEABSY_API_KEY").is_some() {
                continue;
            }
            let err = Api::new(model, None, 1, &BTreeMap::new(), tmp.path())
                .err()
                .unwrap()
                .to_string();
            assert!(err.contains(says), "{model}: {err}");
        }
    }
}
