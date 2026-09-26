//! CAD-129: `cadence test submit|status|log|wait`.

use super::*;

#[derive(Subcommand)]
pub(crate) enum TestAction {
    /// Queue a cargo test in the daemon and print the job id. A cache
    /// hit returns the earlier job id and its age. `--no-cache` forces
    /// a fresh run unless one is already in flight for the same key.
    Submit {
        /// Checkout the daemon will run cargo in.
        #[arg(long)]
        worktree: PathBuf,
        /// Cargo test filter. Empty runs the default selection.
        #[arg(long, default_value = "")]
        filter: String,
        /// `cargo test --all-targets`. A full run holds the queue alone.
        #[arg(long)]
        full: bool,
        /// Ignore a successful cache entry. An in-flight run of the
        /// same key is still joined.
        #[arg(long)]
        no_cache: bool,
        /// Block until the job passes, fails, or is interrupted.
        #[arg(long)]
        wait: bool,
        /// Give up waiting after this many seconds (with `--wait`).
        #[arg(long, default_value_t = 3600)]
        timeout_secs: u64,
    },
    /// Print one job: state, outcome, timings, cache flag.
    Status { id: String },
    /// Print the job's cargo log (the last 256 KiB when it is longer).
    Log { id: String },
    /// Block until the job reaches a terminal state.
    Wait {
        id: String,
        /// Give up after this many seconds.
        #[arg(long, default_value_t = 3600)]
        timeout_secs: u64,
    },
}

pub(super) fn run(state_dir: &Path, action: &TestAction) -> Result<i32> {
    match action {
        TestAction::Submit {
            worktree,
            filter,
            full,
            no_cache,
            wait,
            timeout_secs,
        } => {
            let view = client::rpc(
                state_dir,
                "test_submit",
                json!({
                    "worktree": worktree,
                    "filter": filter,
                    "full": full,
                    "no_cache": no_cache,
                    "rustflags": std::env::var("RUSTFLAGS").unwrap_or_default(),
                    "env": cadence_agent::test_queue::allowlisted_env(std::env::vars()),
                }),
            )?;
            print_json(&view);
            if *wait {
                let id = view["id"].as_str().unwrap_or("");
                return wait_job(state_dir, id, *timeout_secs);
            }
            Ok(0)
        }
        TestAction::Status { id } => {
            print_json(&client::rpc(state_dir, "test_status", json!({"id": id}))?);
            Ok(0)
        }
        TestAction::Log { id } => {
            let view = client::rpc(state_dir, "test_log", json!({"id": id}))?;
            print!("{}", view["log"].as_str().unwrap_or(""));
            Ok(0)
        }
        TestAction::Wait { id, timeout_secs } => wait_job(state_dir, id, *timeout_secs),
    }
}

fn wait_job(state_dir: &Path, id: &str, timeout_secs: u64) -> Result<i32> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let view = client::rpc(state_dir, "test_status", json!({"id": id}))?;
        let state = view["state"].as_str().unwrap_or("");
        if matches!(state, "passed" | "failed" | "interrupted") {
            print_json(&view);
            return Ok(match state {
                "passed" => 0,
                "failed" => view["exit_code"].as_i64().unwrap_or(1).clamp(1, 125) as i32,
                _ => 2,
            });
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(format!(
                "test job {id} still {state} after {timeout_secs}s"
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
