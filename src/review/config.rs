//! `cadence-review.toml` schema, defaults and validation — every step
//! `cadence review` performs is data parsed here. The parent module
//! re-exports the public names so `crate::review::…` paths are
//! unchanged; label helpers and `safe_rel_path` stay `pub(super)` for
//! the parent's report and isolation logic.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Config file `run` reads from the base branch head (never the PR tree
/// or the reviewer's working tree).
pub const CONFIG_FILE: &str = "cadence-review.toml";
/// Required and optional keys, named when the config file is absent.
pub(super) const CONFIG_KEYS: &str = "Required keys: prepare, gates, full_suite, test_globs, \
     test_command, stress_pattern; optional: test_command_lib, [timeouts] \
     prepare_secs gate_secs stress_secs full_secs test_secs git_secs gh_secs";

/// `stress_pattern` accepts a single string or a list of strings; a new
/// test is stressed when its name or added body contains any of them.
#[derive(Clone, Debug, Default)]
pub struct Patterns(pub Vec<String>);

impl<'de> Deserialize<'de> for Patterns {
    fn deserialize<D>(d: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        Ok(match OneOrMany::deserialize(d)? {
            OneOrMany::One(s) => Patterns(vec![s]),
            OneOrMany::Many(v) => Patterns(v),
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Timeouts {
    /// Per `prepare` command [default 900].
    #[serde(default = "t_prepare")]
    pub prepare_secs: u64,
    /// Per `gates` command [default 1800].
    #[serde(default = "t_gate")]
    pub gate_secs: u64,
    /// Per single stress run [default 900].
    #[serde(default = "t_stress")]
    pub stress_secs: u64,
    /// The `full_suite` command [default 3600].
    #[serde(default = "t_full")]
    pub full_secs: u64,
    /// Per isolated `test_command` run [default 900].
    #[serde(default = "t_test")]
    pub test_secs: u64,
    /// Per git operation [default 300].
    #[serde(default = "t_git")]
    pub git_secs: u64,
    /// Per `gh` call [default 60].
    #[serde(default = "t_gh")]
    pub gh_secs: u64,
}

/// The subprocess backend used for the full suite and isolated reruns.
/// Keeping this explicit prevents a nextest suite from being adjudicated by
/// a cargo child with different process and retry semantics.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewBackend {
    #[default]
    Cargo,
    Nextest,
}

/// Machine-readable result format emitted by the configured backend.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResultFormat {
    #[default]
    Cargo,
    Junit,
}

fn default_result_path() -> String {
    "target/nextest/cadence/junit.xml".into()
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerConfig {
    /// Backend used by both `full_suite` and `test_command`.
    #[serde(default)]
    pub backend: ReviewBackend,
    /// How an individual command proves what ran and what failed.
    #[serde(default)]
    pub result_format: ResultFormat,
    /// Repo-relative report path written by a structured backend.
    #[serde(default = "default_result_path")]
    pub result_path: String,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            backend: ReviewBackend::Cargo,
            result_format: ResultFormat::Cargo,
            result_path: default_result_path(),
        }
    }
}

fn t_prepare() -> u64 {
    900
}
fn t_gate() -> u64 {
    1800
}
fn t_stress() -> u64 {
    900
}
fn t_full() -> u64 {
    3600
}
fn t_test() -> u64 {
    900
}
fn t_git() -> u64 {
    300
}
fn t_gh() -> u64 {
    60
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            prepare_secs: t_prepare(),
            gate_secs: t_gate(),
            stress_secs: t_stress(),
            full_secs: t_full(),
            test_secs: t_test(),
            git_secs: t_git(),
            gh_secs: t_gh(),
        }
    }
}

/// Coverage of the configured review suite; CI remains responsible for
/// the complete merge-group inventory when a recipe chooses a baseline.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SuiteScope {
    #[default]
    Full,
    ReviewBaseline,
}

impl SuiteScope {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Full => "Full suite",
            Self::ReviewBaseline => "Review baseline",
        }
    }
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ReviewBaseline => "review-baseline",
        }
    }
}

/// `cadence-review.toml` — every step the verb performs is data here.
#[derive(Clone, Debug, Deserialize)]
pub struct ReviewConfig {
    /// Commands run once in the review checkout before gating (build
    /// steps, dependency links).
    pub prepare: Vec<String>,
    /// Ordered quality gates; the first failure stops the sequence.
    pub gates: Vec<String>,
    /// Configured suite, run once. Legacy key retained for consumers.
    pub full_suite: String,
    /// Describe coverage honestly; older recipes retain full-suite semantics.
    #[serde(default)]
    pub suite_scope: SuiteScope,
    /// Diff paths that count as test files (`*`/`**`/`?` globs).
    pub test_globs: Vec<String>,
    /// How one test runs alone; `{test}` = fn name, `{file}` = diff
    /// path, `{target}` = file stem (cargo `--test <target>`).
    pub test_command: String,
    /// Optional (CAD-799): how a unit test whose file sits outside
    /// `test_globs` runs alone — a `src/` unit test has no `--test
    /// <stem>` target, only a module-qualified name like
    /// `rollout::tests::x`. `{test}` substitutes that full name.
    /// Absent means an unlocatable test stays `inconclusive`, the
    /// pre-CAD-799 behavior.
    #[serde(default)]
    pub test_command_lib: Option<String>,
    /// Substrings marking a new test as "waits on daemon state" —
    /// matched tests are stressed `--stress` times each.
    #[serde(default)]
    pub stress_pattern: Patterns,
    #[serde(default)]
    pub timeouts: Timeouts,
    /// Optional runner contract. The default preserves the historical cargo
    /// text path; nextest activation must opt into a structured report.
    #[serde(default)]
    pub runner: RunnerConfig,
}

impl ReviewConfig {
    /// Load `<root>/cadence-review.toml` from disk; a missing file names
    /// the required keys so a fresh repo can write one without guessing.
    /// `run` never reads the working tree — see `config_at_base`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_FILE);
        if !path.is_file() {
            return Err(Error::rejected(format!(
                "no {CONFIG_FILE} at {} — `cadence review` reads its steps \
                 from that file. {CONFIG_KEYS}",
                root.display()
            )));
        }
        let text = std::fs::read_to_string(&path)?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse and validate config text; `origin` (a path, or
    /// `<sha>:cadence-review.toml`) names the source in every error.
    pub fn parse(text: &str, origin: &str) -> Result<Self> {
        let cfg: ReviewConfig = toml::from_str(text)
            .map_err(|e| Error::rejected(format!("{origin} is not valid TOML: {e}")))?;
        if cfg.gates.is_empty() {
            return Err(Error::rejected(format!(
                "{origin}: `gates` must name at least one command"
            )));
        }
        if cfg.test_globs.is_empty() {
            return Err(Error::rejected(format!(
                "{origin}: `test_globs` must name at least one pattern"
            )));
        }
        if !cfg.test_command.contains("{test}") {
            return Err(Error::rejected(format!(
                "{origin}: `test_command` must contain a {{test}} placeholder"
            )));
        }
        if let Some(lib) = &cfg.test_command_lib {
            if !lib.contains("{test}") {
                return Err(Error::rejected(format!(
                    "{origin}: `test_command_lib` must contain a {{test}} placeholder"
                )));
            }
        }
        if cfg.runner.result_format == ResultFormat::Junit
            && !safe_rel_path(&cfg.runner.result_path)
        {
            return Err(Error::rejected(format!(
                "{origin}: runner.result_path must be a safe repo-relative path"
            )));
        }
        if cfg.runner.backend == ReviewBackend::Nextest {
            if cfg.runner.result_format != ResultFormat::Junit {
                return Err(Error::rejected(format!(
                    "{origin}: nextest requires runner.result_format = 'junit'"
                )));
            }
            let isolation_cmds = std::iter::once(&cfg.test_command)
                .chain(cfg.test_command_lib.iter())
                .collect::<Vec<_>>();
            if !command_mentions_nextest(&cfg.full_suite)
                || !isolation_cmds.iter().all(|c| command_mentions_nextest(c))
            {
                return Err(Error::rejected(format!(
                    "{origin}: nextest backend requires full_suite and every test_command to use scripts/cadence-nextest"
                )));
            }
            if !isolation_cmds
                .iter()
                .all(|c| c.contains("--exact") && c.contains("--"))
            {
                return Err(Error::rejected(format!(
                    "{origin}: nextest test commands must use an exact libtest filter after '--'"
                )));
            }
        }
        Ok(cfg)
    }
}

fn command_mentions_nextest(command: &str) -> bool {
    command.split_whitespace().any(|part| {
        part.trim_matches(|c| c == '\'' || c == '"')
            .ends_with("cadence-nextest")
    })
}

pub(super) fn backend_label(backend: ReviewBackend) -> &'static str {
    match backend {
        ReviewBackend::Cargo => "cargo",
        ReviewBackend::Nextest => "nextest",
    }
}

pub(super) fn result_format_label(format: ResultFormat) -> &'static str {
    match format {
        ResultFormat::Cargo => "cargo-text",
        ResultFormat::Junit => "junit",
    }
}

/// A repo-relative path with no traversal and no shell metacharacters.
pub(super) fn safe_rel_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.split('/').any(|c| c == "..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
}
