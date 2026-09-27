use serde_json::Value;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ---------- CAD-95: shared cargo target dir ----------

/// pm + repo + home + state under one temp dir, a `cli` that runs the
/// binary with the test env, and a `cli_at` that also sets cwd (the
/// doctor checks scan the repo it is launched from).
pub struct SharedTarget {
    pub _tmp: TempDir,
    pub pm_dir: PathBuf,
    pub repo: PathBuf,
    pub home: PathBuf,
    pub state: PathBuf,
    pub bin_dir: PathBuf,
}

impl SharedTarget {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let (pm_dir, repo, home, state) = (
            tmp.path().join("pm"),
            tmp.path().join("repo"),
            tmp.path().join("home"),
            tmp.path().join("state"),
        );
        for dir in [&pm_dir, &repo, &home, &state] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&o.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        // A real repo ignores its build output — `target/` must not
        // read as dirty for `git status` or `issue finish`.
        std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
        // A tiny standalone bin crate so tests can build real per-lane
        // binaries in the worktrees.
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"marker\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
            .parent()
            .unwrap()
            .to_path_buf();
        let s = Self {
            _tmp: tmp,
            pm_dir,
            repo,
            home,
            state,
            bin_dir,
        };
        assert!(s.cli(&["issue", "init"]).0);
        let repo_s = s.repo.canonicalize().unwrap().to_str().unwrap().to_string();
        assert!(
            s.cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s])
                .0
        );
        s
    }

    pub fn cli_at(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        self.cli_at_env(cwd, args, &[])
    }

    pub fn cli_at_env(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    pub fn cli(&self, args: &[&str]) -> (bool, Value) {
        let (code, stdout, stderr) = self.cli_at(&self.repo, args);
        let text = if stdout.is_empty() { stderr } else { stdout };
        (
            code == 0,
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    }

    pub fn set_build_target_dir(&self, value: &str) {
        let yaml_path = self.pm_dir.join("demo/project.yaml");
        let yaml = std::fs::read_to_string(&yaml_path).unwrap();
        // Drop any prior appended `build:` block before adding ours —
        // serde rejects a duplicate field.
        let kept: Vec<&str> = yaml
            .lines()
            .filter(|l| *l != "build:" && !l.starts_with("  target_dir:"))
            .collect();
        std::fs::write(
            &yaml_path,
            format!("{}\nbuild:\n  target_dir: {value}\n", kept.join("\n")),
        )
        .unwrap();
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        assert!(o.status.success());
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "cfg",
            ])
            .output()
            .unwrap();
        assert!(o.status.success());
    }

    pub fn new_issue(&self, title: &str) {
        assert!(self.cli(&["issue", "new", title, "--project", "demo"]).0);
    }

    pub fn worktree_of(&self, id: &str) -> PathBuf {
        let show = self.cli(&["issue", "show", id, "--json"]).1;
        show["refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "worktree")
            .and_then(|r| r["path"].as_str())
            .map(PathBuf::from)
            .expect("worktree ref")
    }

    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// `cargo build` the fixture's `marker` crate inside `wt`.
    pub fn cargo_build(&self, wt: &Path) {
        let o = std::process::Command::new("cargo")
            .arg("build")
            .arg("--quiet")
            .current_dir(wt)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "cargo build: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// Write the marker crate's source so the built binary prints
    /// `marker` — each lane carries a distinct build.
    pub fn set_marker(&self, wt: &Path, marker: &str) {
        std::fs::write(
            wt.join("src/main.rs"),
            format!("fn main() {{ println!(\"{marker}\"); }}\n"),
        )
        .unwrap();
    }
}
