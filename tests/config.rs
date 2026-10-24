use std::{fs, path::PathBuf};

use ai_tools::{
    config::Settings,
    memory::{Knob, claude},
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
        (s.chat.model(), s.chat.background()),
        (claude::model(), true)
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
        "[chat]\nmodel = \"opus\"\nbackground = false\n\n[memory]\nview-lines = 50\n",
    )
    .unwrap();
    fs::write(t.outer().join("ai.toml"), "[memory]\nview-lines = 10\n").unwrap();
    fs::write(
        t.inner().join("ai.toml"),
        "[chat]\nmodel = \"haiku\"\n\n[memory]\nentry-chars = 100\n",
    )
    .unwrap();
    let s = t.load().unwrap();
    assert_eq!(
        (s.chat.model(), s.chat.background()),
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
        ("[chat]\nmodle = \"haiku\"\n", "modle", "unknown field"),
        ("[memroy]\nview-lines = 3\n", "memroy", "unknown field"),
        (
            "[chat]\nbackground = \"maybe\"\n",
            "background",
            "expected a boolean",
        ),
        ("[chat]\nmodel = \" \"\n", "model", "is empty"),
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
        "[chat]\nmodel = \"haiku\"\n\n[memory]\nentry-chars = 100\nview-lines = 10\n",
    )
    .unwrap();
    fs::write(
        t.inner().join("ai.local.toml"),
        "[chat]\nmodel = \"opus\"\n\n[memory]\nentry-chars = 200\n",
    )
    .unwrap();
    let s = t.load().unwrap();
    assert_eq!(
        (s.chat.model(), s.chat.background()),
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
        "[chat]\nmodel = \"opus\"\n",
    )
    .unwrap();
    fs::write(t.inner().join("ai.toml"), "[chat]\nmodel = \"haiku\"\n").unwrap();
    fs::write(t.cwd().join("ai.local.toml"), "[chat]\nmodel = \"fable\"\n").unwrap();
    assert_eq!(t.load().unwrap().chat.model(), "haiku");
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
