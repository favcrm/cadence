use super::{git_at, git_repo};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ---------- CAD-136: report intake ----------

/// pm + two repos (the `cadence` project and a `product` project) +
/// home + state under one temp dir; `cli_at` runs the real binary with
/// cwd control — report routing is decided by kind and cwd, so the
/// fixture keeps both an inside-a-project cwd and a foreign one.
pub struct ReportFx {
    pub _tmp: TempDir,
    pub pm_dir: PathBuf,
    pub notes_dir: PathBuf,
    pub cadence_repo: PathBuf,
    pub product_repo: PathBuf,
    pub foreign_cwd: PathBuf,
    pub home: PathBuf,
    pub state: PathBuf,
    pub bin_dir: PathBuf,
}

impl ReportFx {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let (pm_dir, notes_dir, cadence_repo, product_repo, foreign_cwd, home, state) = (
            tmp.path().join("pm"),
            tmp.path().join("notes"),
            tmp.path().join("cadence-repo"),
            tmp.path().join("product-repo"),
            tmp.path().join("nowhere"),
            tmp.path().join("home"),
            tmp.path().join("state"),
        );
        for dir in [&pm_dir, &notes_dir, &home, &state, &foreign_cwd] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git_repo(&cadence_repo);
        git_repo(&product_repo);
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
            .parent()
            .unwrap()
            .to_path_buf();
        let s = Self {
            _tmp: tmp,
            pm_dir,
            notes_dir,
            cadence_repo,
            product_repo,
            foreign_cwd,
            home,
            state,
            bin_dir,
        };
        assert!(s.cli(&["issue", "init"]).0);
        // init defaults notes_dir to the shared /var/www/agent-notes —
        // a stray real note tagged `Issue: C-1` would flip a derived
        // status and flake these tests, so point it at the temp dir.
        let pm_yaml = s.pm_dir.join("pm.yaml");
        let text = std::fs::read_to_string(&pm_yaml).unwrap();
        let text = text
            .lines()
            .map(|l| {
                if l.starts_with("notes_dir:") {
                    format!("notes_dir: {}", s.notes_dir.display())
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&pm_yaml, format!("{text}\n")).unwrap();
        for (key, prefix, repo) in [
            ("cadence", "C", s.cadence_repo.clone()),
            ("product", "P", s.product_repo.clone()),
        ] {
            let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
            let (ok, out) = s.cli(&[
                "issue", "project", "add", key, "--prefix", prefix, "--repo", &repo_s,
            ]);
            assert!(ok, "project add {key}: {out}");
        }
        s
    }

    pub fn cli(&self, args: &[&str]) -> (bool, Value) {
        self.cli_at(&self.product_repo, args)
    }

    pub fn cli_at(&self, cwd: &Path, args: &[&str]) -> (bool, Value) {
        self.cli_at_env(cwd, args, &[]).2
    }

    /// `(success, stderr, parsed stdout-or-stderr-json)` — stderr kept
    /// separate so refusal tests can assert on the message text.
    pub fn cli_at_env(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (bool, String, (bool, Value)) {
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
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let text = if out.stdout.is_empty() {
            stderr.clone()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            stderr,
            (
                out.status.success(),
                serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
            ),
        )
    }

    pub fn issue_body(&self, project: &str, id: &str) -> String {
        std::fs::read_to_string(self.pm_dir.join(project).join(id).join("issue.md")).unwrap()
    }

    pub fn tracker_log(&self, n: usize) -> String {
        git_at(
            &self.pm_dir,
            &["log", &format!("-{n}"), "--format=%s%n%(trailers)"],
        )
    }
}

// ==================== task reports (CAD-341) ====================

/// A `cadence.report/2` file with the six reflection headings.
pub fn task_report_text(front: &str) -> String {
    let body = [
        "Expected",
        "Evidence",
        "Cause",
        "Correction",
        "Lesson",
        "Next",
    ]
    .iter()
    .map(|h| format!("## {h}\n\n{h} text.\n"))
    .collect::<Vec<_>>()
    .join("\n");
    format!("---\n{front}---\n\n{body}")
}
