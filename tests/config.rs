use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use ai_tools::{
    agent::Counts,
    config::{Agent, Homes, Mention, Settings},
    file::pretty,
    memory::Knob,
    project::Project,
    summariser::Kind,
};
use mti::prelude::*;
use tempfile::TempDir;

/// A user's `ai.toml` path and a project tree `outer/inner/cwd`, no file written.
struct Tree {
    tmp: TempDir,
}

impl Tree {
    fn new() -> Tree {
        let t = Tree {
            tmp: TempDir::new().unwrap(),
        };
        fs::create_dir_all(t.cwd()).unwrap();
        t
    }

    fn user(&self) -> PathBuf {
        self.tmp.path().join("ai.toml")
    }

    fn outer(&self) -> PathBuf {
        self.tmp.path().join("outer")
    }

    fn inner(&self) -> PathBuf {
        self.outer().join("inner")
    }

    fn cwd(&self) -> PathBuf {
        self.inner().join("cwd")
    }

    fn load(&self) -> Settings {
        Settings::load(Some(&self.user()), Some(&self.homes()), &self.cwd())
    }

    /// The user's homes, in the tree: never the real ones.
    fn homes(&self) -> Homes {
        Homes::at(&self.tmp.path().join("home"))
    }

    /// The private settings of the project at `dir`, their directory made, its `ai.toml` given
    /// an id if it holds none.
    fn private(&self, dir: &Path) -> PathBuf {
        private_in(&self.homes(), dir)
    }
}

/// The private settings in `homes` of the project at `dir`, its folder there claimed, its `ai.toml`
/// given an id if it holds none.
fn private_in(homes: &Homes, dir: &Path) -> PathBuf {
    let toml = dir.join("ai.toml");
    let text = fs::read_to_string(&toml).unwrap_or_default();
    if !text.contains("id = ") {
        let id = format!("id = \"{}\"\n", "project".create_type_id::<V7>());
        let text = match text.contains("[project]\n") {
            true => text.replacen("[project]\n", &format!("[project]\n{id}"), 1),
            false => format!("[project]\n{id}\n{text}"),
        };
        fs::write(&toml, text).unwrap();
    }
    ai_tools::repo::ensure(&homes.repo).unwrap();
    let project = Project::find(dir).unwrap().unwrap();
    project.claim(homes).unwrap().join("ai.toml")
}

#[test]
fn without_a_file_every_key_has_its_default() {
    let t = Tree::new();
    let s = t.load();
    assert_eq!(s.summariser().unwrap(), Default::default());
    assert_eq!(s.memory().unwrap(), Default::default());
    assert_eq!(s.chat().unwrap(), Default::default());
    assert_eq!(s.agent_table().unwrap(), Default::default());
    assert_eq!(s.hierarchy().unwrap(), Default::default());
    assert_eq!(s.agent("developer").unwrap(), None);
    assert_eq!(
        (
            s.summariser().unwrap().kind(),
            s.summariser().unwrap().model(),
            s.chat().unwrap().background()
        ),
        (Kind::Claude, "sonnet".to_string(), true)
    );
    assert_eq!(
        (
            s.memory().unwrap().get(Knob::ViewLines),
            s.memory().unwrap().get(Knob::EntryChars)
        ),
        (96, 280)
    );
    assert_eq!(
        s.agent_table().unwrap().view.counts().unwrap(),
        Counts::default()
    );
}

#[test]
fn the_api_backend_takes_a_provider_and_its_model() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[summariser]\nbackend = \"api\"\nmodel = \"deepseek/deepseek-flash\"\n",
    )
    .unwrap();
    let s = t.load();
    assert_eq!(
        (
            s.summariser().unwrap().kind(),
            s.summariser().unwrap().model()
        ),
        (Kind::Api, "deepseek/deepseek-flash".to_string())
    );
}

#[test]
fn an_effort_is_a_genai_effort_or_a_thinking_budget() {
    for (given, kept) in [
        ("\"high\"", "high"),
        ("\" max \"", "max"),
        ("\"none\"", "zero"),
        ("\"zero\"", "zero"),
        ("\"minimal\"", "minimal"),
        ("\"xhigh\"", "xhigh"),
        ("4096", "4096"),
        ("\"4096\"", "4096"),
    ] {
        let t = Tree::new();
        fs::write(t.user(), format!("[summariser]\neffort = {given}\n")).unwrap();
        let effort = t.load().summariser().unwrap().effort.unwrap();
        assert_eq!(effort.reasoning().to_string(), kept, "{given}");
    }
    for given in ["\"\"", "-5", "\"0\"", "5000000000"] {
        let t = Tree::new();
        fs::write(t.user(), format!("[summariser]\neffort = {given}\n")).unwrap();
        let err = t.load().summariser().unwrap_err().to_string();
        assert!(
            err.contains("is no effort") || err.contains("is no budget"),
            "{given}: {err}"
        );
    }
}

#[test]
fn the_claude_backend_takes_only_the_efforts_claude_does() {
    for (given, ok) in [
        ("low", true),
        ("medium", true),
        ("high", true),
        ("xhigh", true),
        ("max", true),
        ("zero", false),
        ("minimal", false),
        ("4096", false),
    ] {
        let t = Tree::new();
        fs::write(t.user(), format!("[summariser]\neffort = \"{given}\"\n")).unwrap();
        let effort = t.load().summariser().unwrap().effort.unwrap();
        match effort.claude() {
            Ok(e) => assert!(ok && e == given, "{given}"),
            Err(err) => assert!(
                !ok && err.to_string().contains("is no effort `claude -p` takes"),
                "{given}: {err}"
            ),
        }
    }
}

#[test]
fn jobs_default_to_eight_and_each_key_merges_on_its_own() {
    let t = Tree::new();
    assert_eq!(t.load().summariser().unwrap().jobs(), 8);
    fs::write(
        t.user(),
        "[summariser]\nbackend = \"api\"\nmodel = \"deepseek/deepseek-flash\"\neffort = \"low\"\njobs = 64\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\neffort = \"max\"\n",
    )
    .unwrap();
    let s = t.load().summariser().unwrap();
    assert_eq!(
        (
            s.kind(),
            s.model(),
            s.effort.as_ref().unwrap().reasoning().to_string(),
            s.jobs()
        ),
        (
            Kind::Api,
            "deepseek/deepseek-flash".into(),
            "max".into(),
            64
        )
    );
}

#[test]
fn a_witness_is_set_by_the_summarisers_keys_merged_key_by_key() {
    let t = Tree::new();
    assert!(t.load().summariser().unwrap().witness().unwrap().is_none());
    fs::write(
        t.user(),
        "[summariser]\nmodel = \"sonnet\"\n\n[summariser.witness]\nbackend = \"api\"\nmodel = \"deepseek/deepseek-flash\"\neffort = \"low\"\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser.witness]\neffort = \"max\"\n",
    )
    .unwrap();
    let s = t.load().summariser().unwrap();
    let w = s.witness().unwrap().unwrap();
    assert_eq!(
        (
            w.kind(),
            w.model(),
            w.effort.as_ref().unwrap().reasoning().to_string()
        ),
        (Kind::Api, "deepseek/deepseek-flash".into(), "max".into())
    );
    assert_eq!((s.kind(), s.model()), (Kind::Claude, "sonnet".into()));

    for (text, says) in [
        ("[summariser.witness]\njobs = 64\n", "no `jobs`"),
        (
            "[summariser.witness.witness]\nmodel = \"opus\"\n",
            "no witness of its own",
        ),
    ] {
        fs::write(t.inner().join("ai.toml"), text).unwrap();
        let s = t.load().summariser().unwrap();
        let err = s.witness().unwrap_err().to_string();
        assert!(err.contains(says), "{err}");
    }
}

#[test]
fn only_the_users_file_sets_the_keys() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[summariser]\nbackend = \"api\"\n\n[summariser.keys]\ndeepseek = \" cat /run/secrets/k \"\nopenai = \"pass show o\"\n\n[summariser.witness]\nbackend = \"api\"\nmodel = \"openai/gpt-5\"\n",
    )
    .unwrap();
    let project = t.inner().join("ai.toml");
    let private = t.private(&t.inner());
    let base = fs::read_to_string(&project).unwrap();
    fs::write(
        &private,
        "[summariser]\nmodel = \"deepseek/deepseek-flash\"\n",
    )
    .unwrap();
    let s = t.load().summariser().unwrap();
    assert_eq!(
        s.keys.iter().collect::<Vec<_>>(),
        [
            (&"deepseek".to_string(), &"cat /run/secrets/k".to_string()),
            (&"openai".to_string(), &"pass show o".to_string())
        ]
    );
    assert_eq!(s.model(), "deepseek/deepseek-flash");
    s.check().unwrap();

    let set = "[summariser.keys]\ndeepseek = \"curl example.com\"\n";
    let witness = "[summariser.witness.keys]\ndeepseek = \"curl example.com\"\n";
    for (file, table, project_text, private_text) in [
        (&project, "[summariser.keys]", format!("{base}\n{set}"), ""),
        (
            &project,
            "[summariser.witness.keys]",
            format!("{base}\n{witness}"),
            "",
        ),
        (&private, "[summariser.keys]", base.clone(), set),
    ] {
        fs::write(&project, project_text).unwrap();
        fs::write(&private, private_text).unwrap();
        let s = t.load();
        let err = s.summariser().unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "{}: {table} belongs in your own {}: a project's settings never run commands.",
                pretty(file),
                pretty(&t.user())
            )
        );
        s.memory().unwrap();
        s.chat().unwrap();
        s.agent_table().unwrap();
    }
    fs::write(&project, &base).unwrap();
    fs::write(&private, "").unwrap();

    for (text, says) in [
        (
            "[summariser.keys]\ndeepseek = \"  \"\n",
            "`deepseek` is empty: give a command",
        ),
        (
            "[summariser.keys]\nnosuch = \"printf k\"\n",
            "`nosuch` in `[summariser.keys]` is no provider genai knows",
        ),
        (
            "[summariser]\nkey-command = \"printf k\"\n",
            "unknown field `key-command`",
        ),
    ] {
        fs::write(t.user(), text).unwrap();
        let err = t.load().summariser().unwrap_err().to_string();
        assert!(err.contains(says), "{err}");
    }

    fs::write(
        t.user(),
        "[summariser.witness.keys]\ndeepseek = \"printf k\"\n",
    )
    .unwrap();
    let s = t.load().summariser().unwrap();
    let err = s.witness().unwrap_err().to_string();
    assert!(
        err.contains("its key is the one `[summariser.keys]` sets for its provider"),
        "{err}"
    );
}

#[test]
fn a_project_s_provider_gets_no_key_set_for_another() {
    if std::env::var_os("HEABSY_API_KEY").is_some() {
        return;
    }
    let t = Tree::new();
    let calls = t.tmp.path().join("calls");
    let ran = t.tmp.path().join("ran");
    fs::write(
        t.user(),
        format!(
            "[summariser]\nbackend = \"api\"\nmodel = \"deepseek/deepseek-flash\"\n\n[summariser.keys]\ndeepseek = \"touch '{}'; printf k-deepseek\"\n",
            ran.display()
        ),
    )
    .unwrap();
    let project = t.inner().join("ai.toml");
    let base = "";

    fs::write(
        &project,
        format!("{base}\n[summariser]\nmodel = \"heabsy/qwen38\"\n"),
    )
    .unwrap();
    let s = t.load().summariser().unwrap();
    for model in [None, Some("heabsy/other")] {
        let err = format!("{:#}", s.backend(model, &calls).err().unwrap());
        assert!(err.contains("Set HEABSY_API_KEY"), "{err}");
    }
    let err = format!("{:#}", s.check().unwrap_err());
    assert!(err.contains("Set HEABSY_API_KEY"), "{err}");
    assert!(!ran.exists(), "deepseek's command ran for another provider");

    fs::write(
        &project,
        format!("{base}\n[summariser.witness]\nbackend = \"api\"\nmodel = \"heabsy/qwen38\"\n"),
    )
    .unwrap();
    let s = t.load().summariser().unwrap();
    for err in [
        format!("{:#}", s.backend(None, &calls).err().unwrap()),
        format!("{:#}", s.check().unwrap_err()),
    ] {
        assert!(err.starts_with("Cannot start the witness"), "{err}");
        assert!(err.contains("Set HEABSY_API_KEY"), "{err}");
    }
}

#[test]
fn the_claude_backend_leaves_the_keys_alone() {
    let t = Tree::new();
    let ran = t.tmp.path().join("ran");
    fs::write(
        t.user(),
        format!(
            "[summariser]\nbackend = \"api\"\nmodel = \"deepseek/deepseek-flash\"\n\n[summariser.keys]\ndeepseek = \"touch '{}'; printf k\"\n\n[summariser.witness]\nbackend = \"api\"\nmodel = \"deepseek/deepseek-flash\"\n",
            ran.display()
        ),
    )
    .unwrap();
    let project = t.inner().join("ai.toml");
    let base = "";
    fs::write(
        &project,
        format!(
            "{base}\n[summariser]\nbackend = \"claude\"\nmodel = \"sonnet\"\n\n[summariser.witness]\nbackend = \"claude\"\nmodel = \"opus\"\n"
        ),
    )
    .unwrap();
    let s = t.load().summariser().unwrap();
    s.check().unwrap();
    // Making the backend runs nothing; it fails only where no `claude` is on PATH.
    if let Err(e) = s.backend(None, &t.tmp.path().join("calls")) {
        assert_eq!(e.to_string(), "No claude on PATH: ai runs on Claude Code.");
    }
    assert!(!ran.exists(), "a key's command ran for Claude Code");
}

#[test]
fn the_nearest_project_file_overrides_the_users_key_by_key() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[chat]\nbackground = false\n\n[summariser]\nmodel = \"opus\"\n\n[memory.view]\nlines = 50\n\n[agent.view]\ntotal = 20\nrecent = 5\n",
    )
    .unwrap();
    fs::write(t.outer().join("ai.toml"), "[memory.view]\nlines = 10\n").unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n\n[memory]\nentry-chars = 100\n\n[agent.view]\ntotal = 16\n",
    )
    .unwrap();
    let s = t.load();
    assert_eq!(
        (
            s.summariser().unwrap().model(),
            s.chat().unwrap().background()
        ),
        ("haiku".into(), false)
    );
    assert_eq!(
        (
            s.memory().unwrap().get(Knob::ViewLines),
            s.memory().unwrap().get(Knob::EntryChars)
        ),
        (50, 100),
        "only the nearest project file counts"
    );
    assert_eq!(
        s.agent_table().unwrap().view.counts().unwrap(),
        Counts {
            total: 16,
            recent: 5,
            ..Counts::default()
        }
    );
}

/// The parts of the settings, by name.
const PARTS: [&str; 7] = [
    "summariser",
    "memory",
    "chat",
    "agent",
    "agents.a",
    "hierarchy",
    "mentions.a",
];

/// Part `name` of `s`, read: nothing, or its error.
fn read(s: &Settings, name: &str) -> anyhow::Result<()> {
    match name {
        "summariser" => s.summariser().map(drop),
        "memory" => s.memory().map(drop),
        "chat" => s.chat().map(drop),
        "agent" => s.agent_table().map(drop),
        "agents.a" => s.agent("a").map(drop),
        "hierarchy" => s.hierarchy().map(drop),
        "mentions.a" => s.mentions("a").map(drop),
        _ => unreachable!(),
    }
}

#[test]
fn a_bad_key_or_value_names_its_file_and_key_and_stops_its_part_alone() {
    let summariser: &[&str] = &["summariser"];
    for (text, key, why, fails) in [
        (
            "[summariser]\nmodle = \"haiku\"\n",
            "modle",
            "unknown field",
            summariser,
        ),
        (
            "[memroy]\nentry-chars = 3\n",
            "memroy",
            "unknown field",
            &PARTS[..],
        ),
        (
            "[memory\nview = 3\n",
            "[memory",
            "unclosed table",
            &PARTS[..],
        ),
        (
            "[memory]\nview-lines = 3\n",
            "view-lines",
            "unknown field",
            &["memory"],
        ),
        (
            "[chat]\nbackground = \"maybe\"\n",
            "background",
            "expected a boolean",
            &["chat"],
        ),
        (
            "[summariser]\nmodel = \" \"\n",
            "model",
            "is empty",
            summariser,
        ),
        (
            "[summariser]\nbackend = \"pi\"\n",
            "backend",
            "unknown variant `pi`, expected `claude` or `api`",
            summariser,
        ),
        (
            "[chat]\nmodel = \"haiku\"\n",
            "model",
            "unknown field",
            &["chat"],
        ),
        (
            "[summariser]\neffort = \"hard\"\n",
            "effort",
            "`hard` is no effort",
            summariser,
        ),
        (
            "[summariser]\neffort = 0\n",
            "effort",
            "0 is no budget",
            summariser,
        ),
        (
            "[summariser]\njobs = 0\n",
            "jobs",
            "positive whole number",
            summariser,
        ),
        (
            "[summariser.witness]\nmodle = \"haiku\"\n",
            "modle",
            "unknown field",
            summariser,
        ),
        (
            "[agents.a]\nharness = \"pi\"\n",
            "harness",
            "unknown field",
            &["agents.a", "hierarchy"],
        ),
        (
            "[agents.a]\nworktree = \"\"\n",
            "worktree",
            "is empty",
            &["agents.a"],
        ),
        (
            "[agents.a]\nreports-to = \"b\"\n",
            "reports-to",
            "expected a sequence",
            &["hierarchy"],
        ),
        (
            "[agents.a]\nreports-to = [\"lead@quran\"]\n",
            "reports-to",
            "'lead@quran' is not a thread name",
            &["hierarchy"],
        ),
        (
            "[agents.a]\nreports-to = [\"b\", \"c\", \"b\"]\n",
            "reports-to",
            "names b twice",
            &["hierarchy"],
        ),
        ("[agents.b]\nworktree = \"\"\n", "worktree", "is empty", &[]),
        (
            "[agent.view]\nlines = 3\n",
            "lines",
            "unknown field",
            &["agent"],
        ),
        (
            "[agent.view]\ntotal = 0\n",
            "total",
            "positive whole number",
            &["agent"],
        ),
        (
            "[agent.view]\nlatest = 0\n",
            "latest",
            "positive whole number",
            &["agent"],
        ),
        (
            "[memory.view]\nlines = 0\n",
            "lines",
            "positive whole number",
            &["memory"],
        ),
        (
            "[memory]\nentry-chars = 513\n",
            "entry-chars",
            "is at most 512",
            &["memory"],
        ),
    ] {
        for at in ["user", "project", "local"] {
            let t = Tree::new();
            let p = match at {
                "user" => t.user(),
                "project" => t.inner().join("ai.toml"),
                _ => {
                    fs::write(t.inner().join("ai.toml"), "").unwrap();
                    t.private(&t.inner())
                }
            };
            fs::write(&p, text).unwrap();
            let s = t.load();
            for part in PARTS {
                match (fails.contains(&part), read(&s, part)) {
                    (true, Err(err)) => {
                        let err = err.to_string();
                        assert!(
                            err.starts_with(&format!("{}: ", p.display()))
                                && err.contains(key)
                                && err.contains(why),
                            "{text} in {at}, {part}: {err}"
                        );
                    }
                    (false, Ok(())) => {}
                    (stops, r) => panic!("{text} in {at}: {part} stops {stops}, read {r:?}"),
                }
            }
        }
    }
}

#[test]
fn a_project_is_found_whatever_its_files_other_tables_hold() {
    let t = Tree::new();
    let p = t.inner().join("ai.toml");
    fs::write(
        &p,
        "[project]\nid = \"project_01m5h1sxw1eh2rcnjyd5hb8fy4\"\n\n[summariser]\nmodle = \"x\"\n\n\
         [agents.a]\nreports-to = [\"a\"]\n",
    )
    .unwrap();
    let found = Project::find(&t.cwd()).unwrap().unwrap();
    assert_eq!(found.id, "project_01m5h1sxw1eh2rcnjyd5hb8fy4");

    fs::write(
        &p,
        "[project]\nid = \"project_01m5h1sxw1eh2rcnjyd5hb8fy4\"\n\n[memroy]\n",
    )
    .unwrap();
    let err = Project::find(&t.cwd()).unwrap_err().to_string();
    assert!(
        err.starts_with(&format!("{}: ", p.display())) && err.contains("memroy"),
        "a table no part has leaves the project in doubt too: {err}"
    );
}

#[test]
fn the_private_file_overrides_its_project_file_key_by_key() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[chat]\nbackground = false\n\n[memory.view]\nlines = 50\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n\n[memory]\nentry-chars = 100\n\n[memory.view]\nlines = 10\n",
    )
    .unwrap();
    fs::write(
        t.private(&t.inner()),
        "[summariser]\nmodel = \"opus\"\n\n[memory]\nentry-chars = 200\n",
    )
    .unwrap();
    let s = t.load();
    assert_eq!(
        (
            s.summariser().unwrap().model(),
            s.chat().unwrap().background()
        ),
        ("opus".into(), false)
    );
    assert_eq!(
        (
            s.memory().unwrap().get(Knob::ViewLines),
            s.memory().unwrap().get(Knob::EntryChars)
        ),
        (10, 200)
    );
}

#[test]
fn only_the_private_file_of_the_nearest_project_counts() {
    let t = Tree::new();
    fs::write(t.outer().join("ai.toml"), "").unwrap();
    fs::write(t.private(&t.outer()), "[summariser]\nmodel = \"opus\"\n").unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n",
    )
    .unwrap();
    let inner = t.private(&t.inner());
    assert_eq!(t.load().summariser().unwrap().model(), "haiku");
    fs::write(&inner, "[summariser]\nmodel = \"fable\"\n").unwrap();
    assert_eq!(t.load().summariser().unwrap().model(), "fable");
    assert_eq!(
        inner,
        t.homes().repo.join("projects/inner/ai.toml"),
        "each project's private settings are in its folder of ~/ai"
    );
}

#[test]
fn a_project_without_an_id_has_no_private_settings() {
    let t = Tree::new();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n",
    )
    .unwrap();
    let p = t.homes().repo.join("projects/inner/ai.toml");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, "[summariser]\nmodel = \"opus\"\n").unwrap();
    assert_eq!(t.load().summariser().unwrap().model(), "haiku");
}

#[test]
fn an_agent_adds_up_instructions_and_overrides_its_worktree() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[agents.stabiliser]\ninstructions = \"/home/me/stabiliser.md\"\nworktree = \"../mine\"\n\n[agents.solo]\nworktree = \"/w\"\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[agents.stabiliser]\ninstructions = \"ai/agents/stabiliser.md\"\nworktree = \"../ai-tools-stabiliser\"\n",
    )
    .unwrap();
    fs::write(
        t.private(&t.inner()),
        "[agents.stabiliser]\ninstructions = \"mine.md\"\nworktree = \"../my-stabiliser\"\n",
    )
    .unwrap();
    let s = t.load();
    assert_eq!(
        s.agent("stabiliser").unwrap(),
        Some(Agent {
            instructions: vec![
                "/home/me/stabiliser.md".into(),
                t.inner().join("ai/agents/stabiliser.md"),
                t.inner().join("mine.md"),
            ],
            worktree: Some(t.inner().join("../my-stabiliser")),
        })
    );
    assert_eq!(
        s.agent("solo").unwrap(),
        Some(Agent {
            instructions: vec![],
            worktree: Some("/w".into()),
        })
    );
    assert_eq!(s.agent("developer").unwrap(), None);
}

#[test]
fn a_users_agent_takes_its_instructions_from_beside_the_users_file() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[agents.reviewer]\ninstructions = \"agents/reviewer.md\"\nworktree = \"../review\"\n",
    )
    .unwrap();
    fs::write(t.inner().join("ai.toml"), "").unwrap();
    assert_eq!(
        t.load().agent("reviewer").unwrap(),
        Some(Agent {
            instructions: vec![t.tmp.path().join("agents/reviewer.md")],
            worktree: Some(t.inner().join("../review")),
        })
    );
}

#[test]
fn outside_a_project_a_relative_worktree_stays_as_written() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[agents.helper]\ninstructions = \"helper.md\"\nworktree = \"w\"\n",
    )
    .unwrap();
    assert_eq!(
        t.load().agent("helper").unwrap(),
        Some(Agent {
            instructions: vec![t.tmp.path().join("helper.md")],
            worktree: Some("w".into()),
        })
    );
}

#[test]
fn the_private_file_holds_no_project_table() {
    let t = Tree::new();
    fs::write(t.inner().join("ai.toml"), "[project]\nname = \"widget\"\n").unwrap();
    let local = t.private(&t.inner());
    fs::write(&local, "[project]\nname = \"mine\"\n").unwrap();
    for part in PARTS {
        assert_eq!(
            read(&t.load(), part).unwrap_err().to_string(),
            format!(
                "{}: [project] belongs in the project's ai.toml, not in its private one.",
                local.display()
            ),
            "{part}"
        );
    }
}

#[test]
fn a_local_file_of_its_own_is_refused() {
    let t = Tree::new();
    fs::write(t.inner().join("ai.toml"), "").unwrap();
    let private = t.private(&t.inner());
    let link = t.inner().join("ai.local.toml");
    fs::remove_file(&link).unwrap();
    fs::write(&link, "[summariser]\nmodel = \"opus\"\n").unwrap();
    for part in PARTS {
        assert_eq!(
            read(&t.load(), part).unwrap_err().to_string(),
            format!(
                "{} is a file of its own. Move it to {}, then run `ai init` to link it.",
                link.display(),
                private.display()
            ),
            "{part}"
        );
    }
}

/// `git args` in `dir`, which must succeed.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_linked_worktree_shares_the_main_checkouts_private_file() {
    let tmp = TempDir::new().unwrap();
    let homes = Homes::at(&tmp.path().join("home"));
    let main = tmp.path().join("main");
    let sub = main.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(
        sub.join("ai.toml"),
        "[project]\nid = \"project_01m5h1sxw1eh2rcnjyd5hb8fy4\"\n\n[summariser]\nmodel = \"haiku\"\n",
    )
    .unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-qm", "init"]);
    git(&main, &["worktree", "add", "-q", "../wt"]);
    let wt = tmp.path().join("wt").join("sub");
    let private = private_in(&homes, &sub);
    assert_eq!(private_in(&homes, &wt), private, "one project, one file");
    fs::write(&private, "[summariser]\nmodel = \"opus\"\n").unwrap();

    let load = |dir: &Path| Settings::load(None, Some(&homes), dir);
    assert_eq!(load(&wt).summariser().unwrap().model(), "opus");
    let p = Project::find(&wt).unwrap().unwrap();
    assert_eq!(
        (p.dir, p.home),
        (wt.clone(), fs::canonicalize(&sub).unwrap())
    );
    assert_eq!(Project::find(&sub).unwrap().unwrap().home, sub);

    Project::init_in(&wt, &homes).unwrap();
    assert_eq!(
        fs::read_link(sub.join("ai.local.toml")).unwrap(),
        private,
        "init in a linked worktree links the private settings from the main checkout"
    );
    assert!(fs::symlink_metadata(wt.join("ai.local.toml")).is_err());
    assert_eq!(load(&wt).summariser().unwrap().model(), "opus");
}

#[test]
fn a_linked_worktree_finds_an_agents_worktree_where_the_main_checkout_does() {
    let tmp = TempDir::new().unwrap();
    let main = fs::canonicalize(tmp.path()).unwrap().join("main");
    let sub = main.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(
        sub.join("ai.toml"),
        "[project]\nid = \"project_01m5h1sxw1eh2rcnjyd5hb8fy4\"\n\n[agents.dev]\nworktree = \"../dev\"\n",
    )
    .unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-qm", "init"]);
    git(&main, &["worktree", "add", "-q", "../wt"]);
    let wt = main.parent().unwrap().join("wt").join("sub");

    let from = |dir: &Path| {
        Settings::load(None, None, dir)
            .agent("dev")
            .unwrap()
            .unwrap()
            .worktree
    };
    assert_eq!(from(&sub), Some(sub.join("../dev")));
    assert_eq!(from(&wt), from(&sub));
}

#[test]
fn reports_to_is_overridden_key_by_key() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[agents.lead]\n\n[agents.engineer]\n\n[agents.developer]\nreports-to = [\"lead\"]\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[agents.developer]\nreports-to = [\"lead\", \"engineer\"]\nworktree = \"../dev\"\n",
    )
    .unwrap();
    let s = t.load();
    let h = s.hierarchy().unwrap();
    assert_eq!(h.superiors("developer"), ["lead", "engineer"]);
    assert_eq!(h.superiors("lead"), [] as [&str; 0]);
    assert_eq!(
        s.agent("developer").unwrap().unwrap().worktree,
        Some(t.inner().join("../dev"))
    );

    fs::write(
        t.private(&t.inner()),
        "[agents.developer]\nreports-to = []\n",
    )
    .unwrap();
    let h = t.load().hierarchy().unwrap();
    assert_eq!(h.superiors("developer"), [] as [&str; 0]);
    assert!(h.below("lead").is_empty());
}

#[test]
fn a_bad_hierarchy_stops_the_hierarchy_alone() {
    for (user, project, err) in [
        (
            "[agents.lead]\nreports-to = [\"lead\"]\n",
            "",
            "Agent lead reports to itself. Take lead out of its reports-to.",
        ),
        (
            "[agents.developer]\nreports-to = [\"lead\"]\n",
            "",
            "Agent developer reports to lead, which no [agents.lead] table defines. Add the \
             table to an ai.toml, or take lead out of developer's reports-to.",
        ),
        (
            "[agents.a]\nreports-to = [\"b\"]\n\n[agents.b]\nreports-to = [\"c\"]\n\n\
             [agents.c]\nreports-to = [\"a\"]\n",
            "",
            "Agents report to each other in a cycle: a -> b -> c -> a. Take one name out of a \
             reports-to.",
        ),
        (
            "[agents.main]\n\n[agents.lead]\nreports-to = [\"main\"]\n\n[agents.developer]\n\
             reports-to = [\"lead\"]\n",
            "[agents.lead]\nreports-to = [\"main\", \"developer\"]\n",
            "Agents report to each other in a cycle: developer -> lead -> developer. Take one \
             name out of a reports-to.",
        ),
    ] {
        let t = Tree::new();
        fs::write(t.user(), user).unwrap();
        fs::write(t.inner().join("ai.toml"), project).unwrap();
        let s = t.load();
        assert_eq!(
            s.hierarchy().unwrap_err().to_string(),
            err,
            "{user}{project}"
        );
        for part in PARTS.into_iter().filter(|p| *p != "hierarchy") {
            assert!(read(&s, part).is_ok(), "{part}: {user}{project}");
        }
    }
}

/// `main` over `quran` and `linguist`; `quran` over `lead`; `lead` over `developer` and
/// `reviewer`; `engineer`, under `main`, also over `developer`.
const TEAM: &str = "[agents.main]\n\n\
    [agents.quran]\nreports-to = [\"main\"]\n\n\
    [agents.linguist]\nreports-to = [\"main\"]\n\n\
    [agents.engineer]\nreports-to = [\"main\"]\n\n\
    [agents.lead]\nreports-to = [\"quran\"]\n\n\
    [agents.developer]\nreports-to = [\"lead\", \"engineer\"]\n\n\
    [agents.reviewer]\nreports-to = [\"lead\"]\n";

#[test]
fn an_agent_reads_every_agent_below_it_through_any_number_of_levels() {
    let t = Tree::new();
    fs::write(t.user(), TEAM).unwrap();
    let h = t.load().hierarchy().unwrap();
    let below = |n: &str| h.below(n).into_iter().collect::<Vec<_>>();
    assert_eq!(
        below("main"),
        [
            "developer",
            "engineer",
            "lead",
            "linguist",
            "quran",
            "reviewer"
        ]
    );
    assert_eq!(below("quran"), ["developer", "lead", "reviewer"]);
    assert_eq!(below("lead"), ["developer", "reviewer"]);
    assert!(
        below("developer").is_empty(),
        "never a sibling or a superior"
    );
    assert!(below("linguist").is_empty());
    assert!(below("nobody").is_empty());
}

#[test]
fn an_agent_with_several_superiors_is_below_each_of_them() {
    let t = Tree::new();
    fs::write(t.user(), TEAM).unwrap();
    let h = t.load().hierarchy().unwrap();
    assert_eq!(h.superiors("developer"), ["lead", "engineer"]);
    assert_eq!(
        h.below("engineer").into_iter().collect::<Vec<_>>(),
        ["developer"]
    );
    assert!(h.below("lead").contains("developer"));
    assert!(
        !h.below("engineer").contains("reviewer"),
        "lead's, not engineer's"
    );
    assert!(!h.below("quran").contains("engineer"), "a sibling of quran");
}

#[test]
fn mentions_name_each_table_and_reports_to_of_an_agent_by_file_and_line() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[chat]\nbackground = false\n\n[agents.dev]\ninstructions = \"dev.md\"\n",
    )
    .unwrap();
    let project = t.inner().join("ai.toml");
    fs::write(
        &project,
        "[summariser]\nmodle = \"bad, but not read\"\n\n\
         [agents.lead]\nharness = \"bad too\"\nreports-to = [\"main\", \"dev\"]\n\n\
         [agents.qa]\nreports-to = [\n  \"dev\",\n]\n\n\
         [agents.ops]\nreports-to = \"dev\"\n\n\
         [agents]\nreviewer = { reports-to = [\"dev\"] }\ndevops = { reports-to = [\"lead\"] }\n",
    )
    .unwrap();
    let local = t.private(&t.inner());
    fs::write(&local, "agents.dev.worktree = \"../dev\"\n").unwrap();
    let at = |path: &Path, line, by: Option<&str>| Mention {
        path: path.to_path_buf(),
        line,
        by: by.map(String::from),
    };
    let line_of = |p: &Path, needle: &str| {
        let text = fs::read_to_string(p).unwrap();
        text.lines().position(|l| l.contains(needle)).unwrap() + 1
    };
    let lines = [
        line_of(&project, "\"main\", \"dev\""),
        line_of(&project, "[agents.qa]") + 1,
        line_of(&project, "reviewer = "),
    ];
    let project = fs::canonicalize(&project).unwrap();
    let got: Vec<Mention> = t
        .load()
        .mentions("dev")
        .unwrap()
        .into_iter()
        .map(|m| Mention {
            path: fs::canonicalize(&m.path).unwrap(),
            ..m
        })
        .collect();
    assert_eq!(
        got,
        [
            at(&fs::canonicalize(t.user()).unwrap(), 4, None),
            at(&project, lines[0], Some("lead")),
            at(&project, lines[1], Some("qa")),
            at(&project, lines[2], Some("reviewer")),
            at(&fs::canonicalize(&local).unwrap(), 1, None),
        ]
    );
    assert!(t.load().mentions("nobody").unwrap().is_empty());
}
