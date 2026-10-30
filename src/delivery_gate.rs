//! Run `scripts/enqueue-reviewed` as the one delivery decision.
//!
//! The hosted check and the enqueue both shell that script. Callers
//! pass a registered repo, a PR number and a full head. The script
//! path, the notes directory, the signing key and the `cadence` the
//! script calls are chosen here, never from the request. A missing or
//! unreadable answer is a refusal: it is never a pass.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};

const STDOUT_CAP: usize = 1024 * 1024;

/// One run of the gate script.
pub struct Run {
    pub script: PathBuf,
    pub cadence: PathBuf,
    pub state_dir: PathBuf,
    pub pm_dir: PathBuf,
    pub notes_dir: PathBuf,
    pub repo: String,
    pub pr: u64,
    pub head: String,
    /// Sign and enqueue. A check passes `--check` and does not use the key.
    pub enqueue: bool,
    pub gate_key: Option<PathBuf>,
    pub timeout: Duration,
}

/// The first project checkout that contains the gate script. App repos
/// do not carry it; the cadence checkout does, and that copy is the
/// decision for every repo.
pub fn find_script(pm_dir: &Path) -> Result<PathBuf> {
    let projects = crate::issue::project::list(pm_dir)?;
    for project in projects {
        for repo in &project.repos {
            let Some(path) = repo.path.as_deref() else {
                continue;
            };
            let script = crate::issue::project::expand_home(path).join("scripts/enqueue-reviewed");
            if script.is_file() {
                return Ok(script);
            }
        }
    }
    Err(Error::rejected(
        "delivery gate: no project checkout has scripts/enqueue-reviewed — nothing was checked",
    ))
}

/// Notes the script reads. The daemon's own environment, never a request field.
pub fn notes_dir() -> PathBuf {
    std::env::var_os("CADENCE_NOTES_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/var/www/agent-notes"))
}

/// The signing key. The state-dir file wins, so a hosted org does not
/// fall through to a laptop key that is not mounted. Absent is a refusal.
pub fn signing_key(state_dir: &Path) -> Result<PathBuf> {
    let local = state_dir.join("delivery-gate/signing_key");
    if local.is_file() {
        return Ok(local);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home).join(".config/cadence/delivery-gate/signing_key");
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(Error::rejected(
        "delivery gate: no signing key at <state>/delivery-gate/signing_key \
         or ~/.config/cadence/delivery-gate/signing_key — nothing was enqueued",
    ))
}

/// Run the script and return its JSON document. A refusal the script
/// states (`ok: false`, exit 1) is that document. A missing document,
/// a non-JSON answer, or an exit that disagrees with `ok` is an error:
/// the caller must not treat it as a pass.
pub fn run(spec: &Run) -> Result<Value> {
    if spec.pr == 0 || !valid_repo(&spec.repo) || !valid_head(&spec.head) || !spec.script.is_file()
    {
        return Err(Error::rejected(
            "delivery gate: repo, pr and head are not a registered pull request \
             at a full head, or the gate script is missing — nothing was checked",
        ));
    }
    if spec.enqueue && spec.gate_key.as_ref().is_none_or(|key| !key.is_file()) {
        return Err(Error::rejected(
            "delivery gate: refusing to enqueue without a signing key",
        ));
    }
    let tmp = TempDir::new()?;
    write_wrapper(&tmp.0, &spec.cadence, &spec.state_dir, &spec.pm_dir)?;
    let mut cmd = Command::new(&spec.script);
    cmd.arg(if spec.enqueue { "--json" } else { "--check" });
    cmd.arg("--repo").arg(&spec.repo);
    cmd.arg("--head").arg(&spec.head);
    cmd.arg("--notes-dir").arg(&spec.notes_dir);
    if let Some(key) = &spec.gate_key {
        if spec.enqueue {
            cmd.arg("--gate-key").arg(key);
        }
    }
    cmd.arg(spec.pr.to_string());
    let mut path = std::ffi::OsString::from(&tmp.0);
    path.push(":");
    if let Some(old) = std::env::var_os("PATH") {
        path.push(old);
    }
    cmd.env("PATH", path);
    cmd.env_remove("CADENCE_ORG");
    cmd.env_remove("CADENCE_ALIAS");
    cmd.env_remove("CADENCE_GRANT_TOKEN");
    cmd.env("CADENCE_STATE_DIR", &spec.state_dir);
    cmd.env("CADENCE_PM_DIR", &spec.pm_dir);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| {
        Error::rejected(format!(
            "delivery gate: the check could not start ({e}) — nothing was decided"
        ))
    })?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_thread = thread::spawn(move || read_capped(stdout));
    let err_thread = thread::spawn(move || read_capped(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > spec.timeout => {
                let _ = Command::new("kill")
                    .args(["-KILL", &format!("-{pid}")])
                    .output();
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::rejected(
                    "delivery gate: the check did not answer in time — nothing was decided",
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(30)),
            Err(e) => {
                return Err(Error::rejected(format!(
                    "delivery gate: the check failed while running ({e}) — nothing was decided"
                )));
            }
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if stdout.len() > STDOUT_CAP || stderr.len() > STDOUT_CAP {
        return Err(Error::rejected(
            "delivery gate: the check's answer was too large — nothing was decided",
        ));
    }
    let code = status.code();
    let text = String::from_utf8_lossy(&stdout);
    let parsed = serde_json::from_str::<Value>(text.trim());
    let Ok(value) = parsed else {
        let why = String::from_utf8_lossy(&stderr);
        let why = why.trim();
        let why = if why.is_empty() {
            "it did not return JSON"
        } else {
            why
        };
        return Err(Error::rejected(format!(
            "delivery gate: the check could not be read ({}) — nothing was decided",
            clip(why, 240)
        )));
    };
    let Some(ok) = value.get("ok").and_then(Value::as_bool) else {
        return Err(Error::rejected(
            "delivery gate: the check's answer has no ok field — nothing was decided",
        ));
    };
    let agrees = match code {
        Some(0) => ok,
        Some(1) => !ok,
        _ => false,
    };
    if !agrees {
        return Err(Error::rejected(
            "delivery gate: the check's exit code disagreed with its answer — nothing was decided",
        ));
    }
    // The head in the answer is the head we asked about. A script that
    // reports a different head is not this check.
    if value.get("head").and_then(Value::as_str) != Some(spec.head.as_str())
        || value.get("pr").and_then(Value::as_u64) != Some(spec.pr)
        || value
            .get("repo")
            .and_then(Value::as_str)
            .is_none_or(|repo| !repo.eq_ignore_ascii_case(&spec.repo))
    {
        return Err(Error::rejected(
            "delivery gate: the check answered for a different pull request — nothing was decided",
        ));
    }
    Ok(value)
}

fn valid_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    ok(owner) && ok(name)
}

fn valid_head(head: &str) -> bool {
    head.len() == 40
        && head
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn clip(text: &str, max: usize) -> String {
    let mut out = text.chars().take(max).collect::<String>();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

fn read_capped(mut pipe: Option<impl Read>) -> Vec<u8> {
    let Some(pipe) = pipe.as_mut() else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if buf.len() > STDOUT_CAP {
            break;
        }
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    buf
}

fn shell_piece(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or_else(|| {
        Error::rejected("delivery gate: a path is not utf-8 — nothing was checked")
    })?;
    if text.is_empty() || text.contains(['\'', '\n', '\r', '\0']) {
        return Err(Error::rejected(
            "delivery gate: a path contains a quote or a newline — nothing was checked",
        ));
    }
    Ok(text.to_string())
}

fn write_wrapper(dir: &Path, cadence: &Path, state: &Path, pm: &Path) -> Result<()> {
    // State and tracker are pinned by the environment. `--org` next to
    // those pins is a conflict, and a saved remote org must not steal
    // the script's `cadence` calls away from this daemon.
    let body = format!(
        "#!/bin/sh\nunset CADENCE_ORG\nunset CADENCE_ALIAS\nunset CADENCE_GRANT_TOKEN\nexport CADENCE_STATE_DIR='{state}'\nexport CADENCE_PM_DIR='{pm}'\nexec '{cadence}' \"$@\"\n",
        state = shell_piece(state)?,
        pm = shell_piece(pm)?,
        cadence = shell_piece(cadence)?,
    );
    let path = dir.join("cadence");
    fs::write(&path, body).map_err(|e| {
        Error::rejected(format!(
            "delivery gate: could not prepare the cadence wrapper ({e})"
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)
            .map_err(|e| Error::rejected(format!("delivery gate: wrapper ({e})")))?
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&path, perms)
            .map_err(|e| Error::rejected(format!("delivery gate: wrapper ({e})")))?;
    }
    Ok(())
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "cadence-gate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir(&dir).map_err(|e| {
            Error::rejected(format!("delivery gate: could not prepare a work dir ({e})"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dir)
                .map_err(|e| Error::rejected(format!("delivery gate: work dir ({e})")))?
                .permissions();
            perms.set_mode(0o700);
            fs::set_permissions(&dir, perms)
                .map_err(|e| Error::rejected(format!("delivery gate: work dir ({e})")))?;
        }
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("enqueue-reviewed");
        fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    fn spec(dir: &Path, script: PathBuf, enqueue: bool) -> Run {
        Run {
            script,
            cadence: PathBuf::from("/usr/bin/true"),
            state_dir: dir.join("state"),
            pm_dir: dir.join("pm"),
            notes_dir: dir.join("notes"),
            repo: "acme/widgets".into(),
            pr: 9,
            head: "a".repeat(40),
            enqueue,
            gate_key: None,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn check_passes_no_author_and_refuses_a_non_json_answer() {
        let dir = tempfile::tempdir().unwrap();
        let argv = dir.path().join("argv.json");
        let script = write_script(
            dir.path(),
            &format!(
                "#!/bin/sh\nprintf '%s' \"$0\" > {q}\nprintf '\\n' >> {q}\nprintf '%s\\n' \"$@\" >> {q}\nprintf '%s' \"$CADENCE_ORG\" >> {q}\nif [ \"$1\" != --check ]; then echo no; exit 1; fi\nprintf '%s' '{{\"ok\":true,\"reasons\":[],\"report\":[],\"rows\":[],\"operator_approval\":false,\"repo\":\"acme/widgets\",\"pr\":9,\"head\":\"{head}\"}}'\n",
                q = argv.display(),
                head = "a".repeat(40),
            ),
        );
        let value = run(&spec(dir.path(), script.clone(), false)).unwrap();
        assert_eq!(value["ok"], true);
        let recorded = fs::read_to_string(&argv).unwrap();
        assert!(recorded.contains("--check"), "{recorded}");
        assert!(recorded.contains("--notes-dir"), "{recorded}");
        assert!(!recorded.contains("--author"), "{recorded}");
        assert!(
            !recorded.contains("fav-limited"),
            "the org rode along: {recorded}"
        );
        let bad = write_script(dir.path(), "#!/bin/sh\necho not-json\nexit 1\n");
        let err = run(&spec(dir.path(), bad, false)).unwrap_err();
        assert!(err.to_string().contains("nothing was decided"), "{err}");
    }

    #[test]
    fn a_disagreed_exit_and_a_timeout_are_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let head = "b".repeat(40);
        let script = write_script(
            dir.path(),
            &format!(
                "#!/bin/sh\nprintf '%s' '{{\"ok\":true,\"reasons\":[],\"report\":[],\"rows\":[],\"repo\":\"acme/widgets\",\"pr\":9,\"head\":\"{head}\"}}'\nexit 1\n"
            ),
        );
        let mut spec = spec(dir.path(), script, false);
        spec.head = head;
        let err = run(&spec).unwrap_err();
        assert!(err.to_string().contains("disagreed"), "{err}");

        let slow = write_script(dir.path(), "#!/bin/sh\nsleep 30\n");
        let mut spec = spec;
        spec.script = slow;
        spec.timeout = Duration::from_millis(200);
        let err = run(&spec).unwrap_err();
        assert!(err.to_string().contains("did not answer"), "{err}");
    }

    #[test]
    fn enqueue_without_a_key_does_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(dir.path(), "#!/bin/sh\necho should-not-run\nexit 0\n");
        let err = run(&spec(dir.path(), script, true)).unwrap_err();
        assert!(err.to_string().contains("signing key"), "{err}");
    }
}
