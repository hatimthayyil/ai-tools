use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use ai_tools::{
    config::{Homes, Settings},
    project::Project,
};
use tempfile::TempDir;

/// A home and a directory `work` beside it, in a temporary directory.
struct Sandbox {
    _tmp: TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for d in ["home/.ai", "work"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        Sandbox { _tmp: tmp, root }
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn user(&self) -> PathBuf {
        self.root.join("home/.ai")
    }

    /// The user's homes, which `ai` finds from this sandbox's `HOME`.
    fn homes(&self) -> Homes {
        Homes::at(&self.root.join("home"))
    }

    /// `ai args` run in `dir`, with this home.
    fn ai(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ai"))
            .current_dir(dir)
            .args(args)
            .env("HOME", self.root.join("home"))
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .unwrap()
    }
}

fn ok(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn fails(o: &Output) -> String {
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8(o.stderr.clone()).unwrap()
}

#[test]
fn init_mints_an_id_once() {
    let s = Sandbox::new();
    let id = ok(&s.ai(&s.work(), &["init"]));
    let id = id.trim_end();
    assert!(
        id.starts_with("project_") && id.len() == "project_".len() + 26,
        "{id}"
    );
    let toml = s.work().join("ai.toml");
    let written = fs::read_to_string(&toml).unwrap();
    assert_eq!(written, format!("[project]\nid = \"{id}\"\n"));
    assert_eq!(ok(&s.ai(&s.work(), &["init"])).trim_end(), id);
    assert_eq!(fs::read_to_string(&toml).unwrap(), written, "unchanged");
}

#[test]
fn init_ignores_the_link_once() {
    let s = Sandbox::new();
    let ignore = s.work().join(".gitignore");
    ok(&s.ai(&s.work(), &["init"]));
    assert_eq!(
        fs::read_to_string(&ignore).unwrap(),
        "/ai.local.toml\n/.ai/local/\n"
    );
    ok(&s.ai(&s.work(), &["init"]));
    assert_eq!(
        fs::read_to_string(&ignore).unwrap(),
        "/ai.local.toml\n/.ai/local/\n"
    );

    for (before, after) in [
        ("/target", "/target\n/ai.local.toml\n/.ai/local/\n"),
        ("/target\n", "/target\n/ai.local.toml\n/.ai/local/\n"),
        ("ai.local.toml\n.ai/local\n", "ai.local.toml\n.ai/local\n"),
        (
            "/ai.local.toml/\n/.ai/local/\n",
            "/ai.local.toml/\n/.ai/local/\n",
        ),
        ("/.ai/\n", "/.ai/\n/ai.local.toml\n/.ai/local/\n"),
    ] {
        fs::write(&ignore, before).unwrap();
        ok(&s.ai(&s.work(), &["init"]));
        assert_eq!(fs::read_to_string(&ignore).unwrap(), after, "{before:?}");
    }
}

#[test]
fn init_links_the_private_settings_once() {
    let s = Sandbox::new();
    let link = s.work().join("ai.local.toml");
    let private = s.homes().repo.join("projects/work/ai.toml");
    ok(&s.ai(&s.work(), &["init"]));
    assert_eq!(fs::read_link(&link).unwrap(), private);
    assert_eq!(fs::read_to_string(&private).unwrap(), "");
    assert!(
        s.homes().repo.join(".git").is_dir(),
        "~/ai becomes a repository"
    );

    fs::write(&link, "[summariser]\nmodel = \"opus\"\n").unwrap();
    ok(&s.ai(&s.work(), &["init"]));
    assert_eq!(
        fs::read_to_string(&private).unwrap(),
        "[summariser]\nmodel = \"opus\"\n",
        "the link writes through, and init keeps what is there"
    );
    assert_eq!(fs::read_link(&link).unwrap(), private);
}

#[test]
fn init_refuses_a_local_file_of_its_own() {
    let s = Sandbox::new();
    let link = s.work().join("ai.local.toml");
    fs::write(&link, "[summariser]\nmodel = \"opus\"\n").unwrap();
    assert_eq!(
        fails(&s.ai(&s.work(), &["init"])),
        format!(
            "{} is a file of its own. Move it to ~/ai/projects/work/ai.toml, then run `ai init` to link it.\n",
            link.display(),
        )
    );
    assert!(!s.work().join("ai.toml").exists(), "nothing is written");
    assert!(!s.homes().repo.exists());
}

#[test]
fn init_keeps_what_the_file_already_says() {
    let s = Sandbox::new();
    let toml = s.work().join("ai.toml");
    let before =
        "# settings\n[summariser]\nmodel = \"haiku\"   # cheap\n\n[project]\nname = \"widget\"\n";
    fs::write(&toml, before).unwrap();
    let id = ok(&s.ai(&s.work(), &["init"]));
    assert_eq!(
        fs::read_to_string(&toml).unwrap(),
        format!("{before}id = \"{}\"\n", id.trim_end())
    );
    let p = Project::find(&s.work()).unwrap().unwrap();
    assert_eq!(
        (p.name.as_str(), p.dir.as_path()),
        ("widget", s.work().as_path())
    );
}

#[test]
fn a_project_is_found_from_below_and_named_by_its_directory() {
    let s = Sandbox::new();
    let id = Project::init_in(&s.work(), &s.homes()).unwrap().id;
    let deep = s.work().join("a/b");
    fs::create_dir_all(&deep).unwrap();
    let p = Project::find(&deep).unwrap().unwrap();
    assert_eq!(
        (p.id.as_str(), p.name.as_str(), p.dir.as_path()),
        (id.as_str(), "work", s.work().as_path())
    );
    fs::write(
        s.work().join("a/ai.toml"),
        "[summariser]\nmodel = \"haiku\"\n",
    )
    .unwrap();
    assert_eq!(
        Project::find(&deep).unwrap(),
        None,
        "the nearest file has no id"
    );
}

#[test]
fn a_bad_id_names_its_file() {
    let s = Sandbox::new();
    let toml = s.work().join("ai.toml");
    for (id, why) in [
        ("thread_01h455vb4pex5vsknk084sn02q", "is not a project's id"),
        ("project_nope", "is not a TypeID"),
    ] {
        fs::write(&toml, format!("[project]\nid = \"{id}\"\n")).unwrap();
        let err = Project::find(&s.work()).unwrap_err().to_string();
        assert!(
            err.starts_with(&format!("{}: [project] id: '{id}' ", toml.display()))
                && err.contains(why),
            "{err}"
        );
    }
}

#[test]
fn the_users_own_file_is_never_a_project_file() {
    let s = Sandbox::new();
    let user = s.user().join("ai.toml");
    fs::write(&user, "[summariser]\nmodel = \"opus\"\n").unwrap();
    let below = s.user().join("chat");
    fs::create_dir_all(&below).unwrap();
    let settings = Settings::load(Some(&user), Some(&s.homes()), &below);
    assert_eq!(settings.summariser().unwrap().model(), "opus");
    assert_eq!(
        fails(&s.ai(&below, &["agent", "list"])),
        "Not in a project: no ai.toml here or above. Run `ai init` in the project's top \
         directory.\n"
    );
    assert_eq!(
        fails(&s.ai(&s.user(), &["init"])),
        "~/.ai/ai.toml holds your own settings; a project's ai.toml sits in its top directory.\n"
    );
    fs::write(&user, "[project]\nname = \"me\"\n").unwrap();
    let err = Settings::load(Some(&user), Some(&s.homes()), &s.work())
        .summariser()
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        format!(
            "{}: [project] belongs in a project's ai.toml, not in your own.",
            user.display()
        )
    );
}

#[test]
fn a_store_ai_0_17_kept_is_refused_by_the_commands_that_open_it_alone() {
    let s = Sandbox::new();
    let id = "project_01h455vb4pex5vsknk084sn02q";
    fs::write(
        s.work().join("ai.toml"),
        format!("[project]\nid = \"{id}\"\n"),
    )
    .unwrap();
    let old = s.work().join(".ai/local");
    fs::create_dir_all(old.join("agents")).unwrap();
    fs::write(old.join("agents/format"), "4\n").unwrap();
    fs::write(old.join("ai.toml"), "[memory.view]\nlines = 9\n").unwrap();

    for args in [&["agent", "list"][..], &["init"]] {
        let e = fails(&s.ai(&s.work(), args));
        assert!(
            e.contains("where ai 0.17 kept them") && e.contains("ai convert"),
            "{args:?}: {e}"
        );
    }
    ok(&s.ai(&s.work(), &["memory", "init"]));
    let view = s.ai(&s.work(), &["memory", "view"]);
    ok(&view);
    let warned = String::from_utf8(view.stderr).unwrap();
    assert_eq!(
        warned
            .matches("are not read until they are converted")
            .count(),
        1,
        "{warned}"
    );
    assert!(
        !s.homes().repo.join("projects").exists(),
        "nothing claimed the project"
    );
}
