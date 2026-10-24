use std::{fs, path::PathBuf};

use ai_tools::{
    config::{Agent, Settings},
    memory::Knob,
    summariser::Kind,
};
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

    fn load(&self) -> anyhow::Result<Settings> {
        Settings::load(Some(&self.user()), &self.cwd())
    }
}

#[test]
fn without_a_file_every_key_has_its_default() {
    let t = Tree::new();
    let s = t.load().unwrap();
    assert_eq!(s, Settings::default());
    assert_eq!(
        (
            s.summariser.kind(),
            s.summariser.model(),
            s.chat.background()
        ),
        (Kind::Claude, "sonnet".to_string(), true)
    );
    assert_eq!(
        (
            s.memory.get(Knob::ViewLines),
            s.memory.get(Knob::EntryChars)
        ),
        (96, 280)
    );
}

#[test]
fn the_nearest_project_file_overrides_the_users_key_by_key() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[chat]\nbackground = false\n\n[summariser]\nmodel = \"opus\"\n\n[memory]\nview-lines = 50\n",
    )
    .unwrap();
    fs::write(t.outer().join("ai.toml"), "[memory]\nview-lines = 10\n").unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n\n[memory]\nentry-chars = 100\n",
    )
    .unwrap();
    let s = t.load().unwrap();
    assert_eq!(
        (s.summariser.model(), s.chat.background()),
        ("haiku".into(), false)
    );
    assert_eq!(
        (
            s.memory.get(Knob::ViewLines),
            s.memory.get(Knob::EntryChars)
        ),
        (50, 100),
        "only the nearest project file counts"
    );
}

#[test]
fn a_bad_key_or_value_names_its_file_and_key() {
    for (text, key, why) in [
        (
            "[summariser]\nmodle = \"haiku\"\n",
            "modle",
            "unknown field",
        ),
        ("[memroy]\nview-lines = 3\n", "memroy", "unknown field"),
        (
            "[chat]\nbackground = \"maybe\"\n",
            "background",
            "expected a boolean",
        ),
        ("[summariser]\nmodel = \" \"\n", "model", "is empty"),
        (
            "[summariser]\nbackend = \"api\"\n",
            "backend",
            "unknown variant `api`, expected `claude`",
        ),
        ("[chat]\nmodel = \"haiku\"\n", "model", "unknown field"),
        ("[agents.a]\nharness = \"pi\"\n", "harness", "unknown field"),
        ("[agents.a]\nworktree = \"\"\n", "worktree", "is empty"),
        (
            "[memory]\nview-lines = 0\n",
            "view-lines",
            "positive whole number",
        ),
        (
            "[memory]\nentry-chars = 513\n",
            "entry-chars",
            "is at most 512",
        ),
    ] {
        for at in ["user", "project", "local"] {
            let t = Tree::new();
            let p = match at {
                "user" => t.user(),
                "project" => t.inner().join("ai.toml"),
                _ => {
                    fs::write(t.inner().join("ai.toml"), "").unwrap();
                    t.inner().join("ai.local.toml")
                }
            };
            fs::write(&p, text).unwrap();
            let err = t.load().unwrap_err().to_string();
            assert!(
                err.starts_with(&format!("{}: ", p.display()))
                    && err.contains(key)
                    && err.contains(why),
                "{text}: {err}"
            );
        }
    }
}

#[test]
fn the_local_file_overrides_its_project_file_key_by_key() {
    let t = Tree::new();
    fs::write(
        t.user(),
        "[chat]\nbackground = false\n\n[memory]\nview-lines = 50\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n\n[memory]\nentry-chars = 100\nview-lines = 10\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.local.toml"),
        "[summariser]\nmodel = \"opus\"\n\n[memory]\nentry-chars = 200\n",
    )
    .unwrap();
    let s = t.load().unwrap();
    assert_eq!(
        (s.summariser.model(), s.chat.background()),
        ("opus".into(), false)
    );
    assert_eq!(
        (
            s.memory.get(Knob::ViewLines),
            s.memory.get(Knob::EntryChars)
        ),
        (10, 200)
    );
}

#[test]
fn only_the_local_file_beside_the_nearest_project_file_counts() {
    let t = Tree::new();
    fs::write(t.outer().join("ai.toml"), "").unwrap();
    fs::write(
        t.outer().join("ai.local.toml"),
        "[summariser]\nmodel = \"opus\"\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n",
    )
    .unwrap();
    fs::write(
        t.cwd().join("ai.local.toml"),
        "[summariser]\nmodel = \"fable\"\n",
    )
    .unwrap();
    assert_eq!(t.load().unwrap().summariser.model(), "haiku");
}

#[test]
fn an_agent_merges_key_by_key_with_paths_from_the_project_root() {
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
        t.inner().join("ai.local.toml"),
        "[agents.stabiliser]\nworktree = \"../my-stabiliser\"\n",
    )
    .unwrap();
    let s = t.load().unwrap();
    assert_eq!(
        s.agent("stabiliser"),
        Some(&Agent {
            instructions: Some(t.inner().join("ai/agents/stabiliser.md")),
            worktree: Some(t.inner().join("../my-stabiliser")),
        })
    );
    assert_eq!(
        s.agent("solo"),
        Some(&Agent {
            instructions: None,
            worktree: Some("/w".into()),
        })
    );
    assert_eq!(s.agent("developer"), None);
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
        t.load().unwrap().agent("reviewer"),
        Some(&Agent {
            instructions: Some(t.tmp.path().join("agents/reviewer.md")),
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
        t.load().unwrap().agent("helper"),
        Some(&Agent {
            instructions: Some(t.tmp.path().join("helper.md")),
            worktree: Some("w".into()),
        })
    );
}

#[test]
fn the_local_file_holds_no_project_table() {
    let t = Tree::new();
    fs::write(t.inner().join("ai.toml"), "[project]\nname = \"widget\"\n").unwrap();
    let local = t.inner().join("ai.local.toml");
    fs::write(&local, "[project]\nname = \"mine\"\n").unwrap();
    assert_eq!(
        t.load().unwrap_err().to_string(),
        format!(
            "{}: [project] belongs in the project's ai.toml, not in ai.local.toml.",
            local.display()
        )
    );
}
