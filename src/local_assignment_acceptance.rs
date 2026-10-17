//! Consumer-visible local-assignment transitions. The board is a small
//! state machine for the five hosted routes, not an echo of the request.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use super::reporting::store_kickoff_binding;
use super::{CoordHttp, Coordinator, Kickoff, Reply, VERSION};
use crate::error::{Error, Result};

const ORG: &str = "ws_cad1460";
const ISSUE: &str = "CAD1460";
const ME: &str = "user_me";
const THEM: &str = "user_them";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const KICKOFF: &str = "hosted local assignment";
const ENDPOINT: &str = "https://board.test";

#[derive(Clone)]
struct Script(Arc<Mutex<Board>>);

struct Call {
    method: String,
    path: String,
    body: Value,
}

#[derive(Clone)]
struct Row {
    assignment_id: String,
    issue_id: String,
    action: String,
    state: String,
    generation: u64,
    claim_id: Option<String>,
    target: Option<String>,
    created_by: String,
    client_agent_id: Option<String>,
    idempotency_key: Option<String>,
    fingerprint: Option<String>,
    text: Option<String>,
    head: Option<String>,
}

struct Board {
    rows: Vec<Row>,
    list_snapshot: Option<Vec<Row>>,
    /// Claim response returned instead of the live row, for a delayed body.
    claim_snapshot: Option<Row>,
    calls: Vec<Call>,
    next_assignment: u64,
    next_claim: u64,
    fail_reassign: bool,
}

impl Script {
    fn new(board: Board) -> Self {
        Self(Arc::new(Mutex::new(board)))
    }

    fn calls(&self) -> Vec<Call> {
        self.0.lock().expect("board").calls.clone()
    }

    fn row(&self, assignment: &str) -> (String, u64, Option<String>) {
        let board = self.0.lock().expect("board");
        let row = board
            .rows
            .iter()
            .find(|row| row.assignment_id == assignment)
            .expect("row");
        (row.state.clone(), row.generation, row.claim_id.clone())
    }
}

impl Call {
    fn is_create(&self) -> bool {
        self.method == "POST" && self.path == base()
    }

    fn is_suffix(&self, suffix: &str) -> bool {
        self.method == "POST" && self.path.ends_with(suffix)
    }
}

impl Clone for Call {
    fn clone(&self) -> Self {
        Self {
            method: self.method.clone(),
            path: self.path.clone(),
            body: self.body.clone(),
        }
    }
}

fn base() -> String {
    format!("/__platform/hosted-cadence/{ORG}/local-assignments")
}

impl Row {
    fn live(&self) -> bool {
        !matches!(self.state.as_str(), "result" | "failed" | "cancelled")
    }

    fn json(&self) -> Value {
        json!({
            "version": VERSION,
            "assignment_id": self.assignment_id,
            "organization_id": ORG,
            "issue_id": self.issue_id,
            "action": self.action,
            "execution": "local",
            "state": self.state,
            "generation": self.generation,
            "claim_id": self.claim_id,
            "target": self.target,
            "created_by_subject_id": self.created_by,
            "client_agent_id": self.client_agent_id,
            "guidance": Value::Null,
            "reported_head_sha": self.head,
            "text": self.text,
        })
    }

    fn claimed_json(&self) -> Value {
        let mut value = self.json();
        value["references"] = json!({"issue_id": self.issue_id});
        value
    }
}

impl Board {
    fn empty() -> Self {
        Self {
            rows: Vec::new(),
            list_snapshot: None,
            claim_snapshot: None,
            calls: Vec::new(),
            next_assignment: 1,
            next_claim: 1,
            fail_reassign: false,
        }
    }

    fn seed(
        &mut self,
        action: &str,
        state: &str,
        target: Option<&str>,
        created_by: &str,
    ) -> String {
        self.next_assignment += 1;
        let assignment_id = format!("hla_00000000-0000-4000-8000-{:012x}", self.next_assignment);
        self.rows.push(Row {
            assignment_id: assignment_id.clone(),
            issue_id: ISSUE.to_string(),
            action: action.to_string(),
            state: state.to_string(),
            generation: 1,
            claim_id: None,
            target: target.map(str::to_string),
            created_by: created_by.to_string(),
            client_agent_id: None,
            idempotency_key: None,
            fingerprint: None,
            text: None,
            head: None,
        });
        assignment_id
    }

    fn handle(&mut self, method: &str, path: &str, body: Option<Value>) -> Result<Reply> {
        let body = body.unwrap_or(Value::Null);
        self.calls.push(Call {
            method: method.to_string(),
            path: path.to_string(),
            body: body.clone(),
        });
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        if method == "GET" && route == base() {
            let state = query.strip_prefix("state=");
            let rows = self.list_snapshot.as_deref().unwrap_or(&self.rows);
            return Ok(Reply {
                status: 200,
                body: json!({
                    "version": VERSION,
                    "assignments": rows.iter().rev()
                        .filter(|row| state.is_none_or(|state| row.state == state))
                        .take(50).map(Row::json).collect::<Vec<_>>(),
                }),
            });
        }
        if method == "POST" && path == base() {
            return Ok(self.create(&body));
        }
        if method == "POST" {
            if let Some(rest) = path.strip_prefix(&format!("{}/", base())) {
                if let Some(id) = rest.strip_suffix("/claim") {
                    return Ok(self.claim(id, &body));
                }
                if let Some(id) = rest.strip_suffix("/report") {
                    return Ok(self.report(id, &body));
                }
                if let Some(id) = rest.strip_suffix("/reassign") {
                    return Ok(self.reassign(id, &body));
                }
            }
        }
        Ok(Reply {
            status: 404,
            body: json!({"code": "api_missing"}),
        })
    }

    fn create(&mut self, body: &Value) -> Reply {
        if body.get("identity_kind").and_then(Value::as_str) != Some("local_worker")
            || body.get("execution").and_then(Value::as_str) != Some("local")
            || body.get("version").and_then(Value::as_str) != Some(VERSION)
            || body.get("organization_id").and_then(Value::as_str) != Some(ORG)
        {
            return http(400, "invalid");
        }
        if forbidden(body) {
            return http(400, "invalid");
        }
        let Some(issue) = body.get("issue_id").and_then(Value::as_str) else {
            return http(400, "invalid");
        };
        let Some(action) = body.get("action").and_then(Value::as_str) else {
            return http(400, "invalid");
        };
        if !matches!(action, "dispatch" | "start") {
            return http(400, "invalid");
        }
        let target = match body.get("target") {
            None => None,
            Some(Value::String(target)) if !target.is_empty() => Some(target.clone()),
            _ => return http(400, "invalid"),
        };
        let Some(key) = body.get("idempotency_key").and_then(Value::as_str) else {
            return http(400, "invalid");
        };
        let fingerprint = body.to_string();
        if let Some(row) = self
            .rows
            .iter()
            .find(|row| row.idempotency_key.as_deref() == Some(key))
        {
            if row.fingerprint.as_deref() == Some(fingerprint.as_str()) {
                return Reply {
                    status: 200,
                    body: row.json(),
                };
            }
            return http(409, "idempotency_conflict");
        }
        if self
            .rows
            .iter()
            .any(|row| row.issue_id == issue && row.live())
        {
            return http(409, "assignment_live");
        }
        self.next_assignment += 1;
        let row = Row {
            assignment_id: format!("hla_{:032x}", self.next_assignment),
            issue_id: issue.to_string(),
            action: action.to_string(),
            state: if target.is_none() {
                "waiting".to_string()
            } else {
                "queued".to_string()
            },
            generation: 1,
            claim_id: None,
            target,
            created_by: ME.to_string(),
            client_agent_id: None,
            idempotency_key: Some(key.to_string()),
            fingerprint: Some(fingerprint),
            text: None,
            head: None,
        };
        let reply = Reply {
            status: 201,
            body: row.json(),
        };
        self.rows.push(row);
        reply
    }

    fn claim(&mut self, id: &str, body: &Value) -> Reply {
        if let Some(row) = self
            .claim_snapshot
            .clone()
            .filter(|row| row.assignment_id == id)
        {
            return Reply {
                status: 200,
                body: row.claimed_json(),
            };
        }
        let Some(index) = self.rows.iter().position(|row| row.assignment_id == id) else {
            return http(404, "api_missing");
        };
        let generation = body.get("expected_generation").and_then(Value::as_u64);
        let client = body.get("client_agent_id").and_then(Value::as_str);
        if generation != Some(self.rows[index].generation) {
            return http(409, "fenced");
        }
        let Some(client) = client else {
            return http(400, "invalid");
        };
        if self.rows[index].state == "running"
            && self.rows[index].client_agent_id.as_deref() == Some(client)
        {
            return Reply {
                status: 200,
                body: self.rows[index].claimed_json(),
            };
        }
        if self.rows[index].state != "queued" {
            return http(409, "not_claimable");
        }
        self.next_claim += 1;
        let claim_id = format!("hlc_{:032x}", self.next_claim);
        let row = &mut self.rows[index];
        row.state = "running".to_string();
        row.claim_id = Some(claim_id);
        row.client_agent_id = Some(client.to_string());
        Reply {
            status: 200,
            body: row.claimed_json(),
        }
    }

    fn report(&mut self, id: &str, body: &Value) -> Reply {
        let Some(row) = self.rows.iter_mut().find(|row| row.assignment_id == id) else {
            return http(404, "api_missing");
        };
        let claim = body.get("claim_id").and_then(Value::as_str);
        let generation = body.get("expected_generation").and_then(Value::as_u64);
        let state = body.get("state").and_then(Value::as_str).unwrap_or("");
        if claim != row.claim_id.as_deref() || generation != Some(row.generation) {
            return Reply {
                status: 409,
                body: json!({"code": "fenced", "generation": row.generation}),
            };
        }
        if matches!(row.state.as_str(), "result" | "failed" | "cancelled") {
            return Reply {
                status: 409,
                body: json!({"code": "terminal", "generation": row.generation}),
            };
        }
        if !matches!(state, "running" | "blocked" | "failed" | "result") {
            return http(400, "invalid");
        }
        if state == "result" {
            match body.get("reported_head_sha").and_then(Value::as_str) {
                Some(sha) if is_sha(sha) => row.head = Some(sha.to_string()),
                _ => return http(400, "invalid"),
            }
        }
        row.state = state.to_string();
        if let Some(text) = body.get("text").and_then(Value::as_str) {
            row.text = Some(text.to_string());
        }
        Reply {
            status: 200,
            body: row.json(),
        }
    }

    fn reassign(&mut self, id: &str, body: &Value) -> Reply {
        let Some(index) = self.rows.iter().position(|row| row.assignment_id == id) else {
            return http(404, "api_missing");
        };
        let generation = body.get("expected_generation").and_then(Value::as_u64);
        if generation != Some(self.rows[index].generation) {
            return http(409, "fenced");
        }
        if self.fail_reassign {
            self.fail_reassign = false;
            return http(500, "unavailable");
        }
        let cancel = body.get("cancel").and_then(Value::as_bool).unwrap_or(false);
        let target = if cancel {
            None
        } else {
            let Some(target) = body.get("target").and_then(Value::as_str) else {
                return http(400, "invalid");
            };
            Some(target.to_string())
        };
        let row = &mut self.rows[index];
        row.generation += 1;
        row.claim_id = None;
        row.client_agent_id = None;
        row.text = None;
        row.head = None;
        if cancel {
            row.state = "cancelled".to_string();
        } else {
            row.target = target;
            row.state = "queued".to_string();
        }
        Reply {
            status: 200,
            body: row.json(),
        }
    }
}

fn http(status: u16, code: &str) -> Reply {
    Reply {
        status,
        body: json!({"code": code, "generation": 1}),
    }
}

fn forbidden(body: &Value) -> bool {
    const KEYS: &[&str] = &[
        "path",
        "repo",
        "cwd",
        "worktree",
        "filesystem",
        "bundle",
        "result_custody",
        "presence",
        "attempt",
        "target_user_id",
    ];
    body.as_object()
        .is_some_and(|object| object.keys().any(|key| KEYS.contains(&key.as_str())))
}

fn is_sha(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn private_dir() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .arg("-c")
        .arg("user.email=cadence@example.com")
        .arg("-c")
        .arg("user.name=Cadence")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn checkout(root: &Path) {
    git(root, &["init", "-b", "main"]);
    fs::write(root.join("README"), "checkout\n").expect("readme");
    git(root, &["add", "README"]);
    git(root, &["commit", "-m", "init"]);
    let lane = crate::worktree::layout::worktree_dir(root, "cad1460-work");
    fs::create_dir_all(&lane).expect("lane");
    git(
        &lane,
        &[
            "init",
            "-b",
            &crate::worktree::layout::branch("cad1460-work"),
        ],
    );
    fs::write(lane.join("note"), "lane\n").expect("note");
    git(&lane, &["add", "note"]);
    git(&lane, &["commit", "-m", "lane"]);
}

fn open(script: &Script, ledger: &Path) -> Coordinator<Script> {
    Coordinator::open(
        script.clone(),
        ORG.to_string(),
        ledger.to_path_buf(),
        Some(ME.to_string()),
        Some(ENDPOINT.to_string()),
        ENDPOINT.to_string(),
    )
    .expect("coordinator")
}

fn deliver_to(state: PathBuf) -> impl FnMut(&Kickoff) -> Result<Value> {
    move |kickoff| {
        store_kickoff_binding(&state, "acme", ENDPOINT, kickoff, KICKOFF)?;
        Ok(json!({"message": kickoff.message_id}))
    }
}

fn ledger_file(dir: &Path) -> Value {
    let bytes = fs::read(dir.join(format!("local-assign-{ORG}.json"))).expect("ledger");
    serde_json::from_slice(&bytes).expect("ledger json")
}

fn attempt<'a>(ledger: &'a Value, assignment: &str) -> &'a Value {
    ledger["attempts"]
        .as_object()
        .expect("attempts")
        .values()
        .find(|attempt| attempt["assignment_id"].as_str() == Some(assignment))
        .expect("attempt")
}

fn actor_message(value: &Value) -> String {
    value["actor"]["message"]
        .as_str()
        .or_else(|| value["claimed"][0]["actor"]["message"].as_str())
        .expect("actor message")
        .to_string()
}

#[test]
fn untargeted_dispatch_stays_waiting_for_this_machine() {
    let auth = private_dir();
    let script = Script::new(Board::empty());
    let mut coord = open(&script, auth.path());
    let value = coord.dispatch(ISSUE, None).expect("dispatch");
    assert_eq!(value["state"], "waiting");
    assert!(value["target"].is_null());
    assert_eq!(value["execution"], "local");
    assert_eq!(value["guidance"]["code"], "connect_local_agent");
    let text = value["guidance"]["text"].as_str().expect("guidance");
    assert!(text.contains("hosted runner"));
    assert!(text.contains("local agent"));
    let calls = script.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].is_create());
    assert_eq!(calls[0].body["identity_kind"], "local_worker");
    assert_eq!(calls[0].body["execution"], "local");
    assert!(calls[0].body.get("target").is_none());
    assert!(calls[0].body.get("worktree").is_none());
}

#[test]
fn start_adopts_this_users_waiting_assignment() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let script = Script::new(Board::empty());
    let mut coord = open(&script, auth.path());
    let mut idle = |_: &Kickoff| -> Result<Value> {
        panic!("a waiting assignment must not message an actor");
    };
    let waiting = coord
        .start_here(ISSUE, repo.path(), None, &mut idle)
        .expect("waiting start");
    assert_eq!(waiting["state"], "waiting");
    assert!(waiting["actor"].is_null());
    assert_eq!(waiting["guidance"]["code"], "connect_local_agent");
    coord.bind_alias("worker").expect("alias");
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let started = coord
        .start_here(ISSUE, repo.path(), None, &mut deliver)
        .expect("adopt");
    assert_eq!(started["state"], "running");
    let message = actor_message(&started);
    let again = coord
        .start_here(ISSUE, repo.path(), None, &mut deliver)
        .expect("reconnect");
    assert_eq!(again["state"], "running");
    assert_eq!(again["actor"]["state"], "already_delivered");
    assert_eq!(actor_message(&again), message);
    let calls = script.calls();
    assert_eq!(calls.iter().filter(|call| call.is_create()).count(), 1);
    let reassign: Vec<_> = calls
        .iter()
        .filter(|call| call.is_suffix("/reassign"))
        .collect();
    assert_eq!(reassign.len(), 1);
    assert_eq!(reassign[0].body["target"], ME);
    assert!(reassign[0].body.get("cancel").is_none());
    assert_eq!(calls[0].body["identity_kind"], "local_worker");
    assert!(calls[0].body.get("target").is_none());
}

#[test]
fn connect_adopts_waiting_work_and_reuses_one_kickoff_id() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let script = Script::new(Board::empty());
    let mut coord = open(&script, auth.path());
    let waiting = coord.dispatch(ISSUE, None).expect("queue");
    assert_eq!(waiting["state"], "waiting");
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let connected = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("connect");
    assert_eq!(connected["claimed"][0]["state"], "running");
    let message = actor_message(&connected);
    let again = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("reconnect");
    assert_eq!(again["claimed"][0]["actor"]["state"], "already_delivered");
    assert_eq!(actor_message(&again), message);
    let calls = script.calls();
    assert_eq!(calls.iter().filter(|call| call.is_create()).count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.is_suffix("/reassign"))
            .count(),
        1
    );
    let ledger = ledger_file(auth.path());
    let origin = &ledger["origins"][&message];
    assert_eq!(origin["claim_id"], connected["claimed"][0]["claim_id"]);
    assert_eq!(
        attempt(
            &ledger,
            connected["claimed"][0]["assignment_id"].as_str().unwrap()
        )["assignment_id"],
        connected["claimed"][0]["assignment_id"]
    );
}

#[test]
fn teammate_assignment_is_claimed_for_the_named_user() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    board.seed("dispatch", "queued", Some(ME), THEM);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let connected = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("claim teammate row");
    assert_eq!(connected["claimed"].as_array().expect("claimed").len(), 1);
    assert_eq!(connected["claimed"][0]["state"], "running");
    assert_eq!(connected["claimed"][0]["target"], ME);
    let calls = script.calls();
    assert_eq!(calls.iter().filter(|call| call.is_create()).count(), 0);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.is_suffix("/reassign"))
            .count(),
        0
    );
    assert_eq!(
        calls.iter().filter(|call| call.is_suffix("/claim")).count(),
        1
    );
    let assignment = connected["claimed"][0]["assignment_id"].as_str().unwrap();
    let ledger = ledger_file(auth.path());
    assert_eq!(
        attempt(&ledger, assignment)["claim_id"],
        connected["claimed"][0]["claim_id"]
    );
}

#[test]
fn lost_receipt_keeps_the_reserved_id_and_resumes_the_same_claim() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), THEM);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = auth.path().to_path_buf();
    let mut deliver = {
        let seen = Arc::clone(&seen);
        let ledger = auth.path().to_path_buf();
        move |kickoff: &Kickoff| {
            let saved = ledger_file(&ledger);
            assert_eq!(
                attempt(&saved, kickoff.assignment_id)["kickoff_reserved"].as_str(),
                Some(kickoff.message_id),
                "kickoff id must be durable before the actor is messaged"
            );
            store_kickoff_binding(&state, "acme", ENDPOINT, kickoff, KICKOFF)?;
            seen.lock()
                .expect("seen")
                .push(kickoff.message_id.to_string());
            if seen.lock().expect("seen").len() == 1 {
                return Err(Error::rejected(
                    "lost response before the actor receipt was saved",
                ));
            }
            Ok(json!({"message": kickoff.message_id}))
        }
    };
    coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("partial batch records the refused handoff");
    let (state_now, _, claim) = script.row(&assignment);
    assert_eq!(state_now, "blocked");
    let claim = claim.expect("claim survived the lost receipt");
    let resumed = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("resume");
    assert_eq!(resumed["claimed"][0]["state"], "running");
    assert_eq!(resumed["claimed"][0]["claim_id"], claim);
    let ids = seen.lock().expect("seen");
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
    assert_eq!(actor_message(&resumed), ids[0]);
}

#[test]
fn cancel_waits_for_the_board_before_fencing_the_claim() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    board.fail_reassign = true;
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let connected = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("claim");
    let message = actor_message(&connected);
    let claim = connected["claimed"][0]["claim_id"]
        .as_str()
        .unwrap()
        .to_string();
    let failed = coord
        .reassign(&assignment, None, true)
        .expect_err("board has not accepted cancel");
    assert_eq!(failed.code(), Some("unavailable"));
    let ledger = ledger_file(auth.path());
    let saved = attempt(&ledger, &assignment);
    assert_eq!(saved["fenced"], false);
    assert_eq!(saved["claim_id"], claim);
    assert!(ledger["origins"].get(&message).is_some());
    let (state_now, _, board_claim) = script.row(&assignment);
    assert_eq!(state_now, "running");
    assert_eq!(board_claim.as_deref(), Some(claim.as_str()));
    let cancelled = coord.reassign(&assignment, None, true).expect("cancel");
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["stopped"], false);
    let replays: Vec<_> = script
        .calls()
        .into_iter()
        .filter(|call| call.is_suffix("/reassign"))
        .collect();
    assert_eq!(replays.len(), 2);
    assert_eq!(
        replays[0].body["operation_id"],
        replays[1].body["operation_id"]
    );
    assert_eq!(
        replays[0].body["expected_generation"],
        replays[1].body["expected_generation"]
    );
    let ledger = ledger_file(auth.path());
    let saved = attempt(&ledger, &assignment);
    assert_eq!(saved["fenced"], true);
    assert!(saved["claim_id"].is_null());
    assert_eq!(ledger["origins"][&message]["claim_id"], claim);
    assert!(ledger["origins"][&message].get("message_id").is_some());
}

#[test]
fn report_posts_the_origin_claim_after_the_ledger_claim_changes() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let connected = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("claim");
    let message = actor_message(&connected);
    let ledger = ledger_file(auth.path());
    let origin_claim = ledger["origins"][&message]["claim_id"]
        .as_str()
        .unwrap()
        .to_string();
    let origin_generation = ledger["origins"][&message]["generation"].as_u64().unwrap();
    let mut edited = ledger;
    {
        let attempt = edited["attempts"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .find(|attempt| attempt["assignment_id"].as_str() == Some(assignment.as_str()))
            .unwrap();
        attempt["claim_id"] = json!("hlc_00000000000000000000000000000099");
        attempt["generation"] = json!(9);
    }
    write_private(
        &auth.path().join(format!("local-assign-{ORG}.json")),
        &serde_json::to_vec_pretty(&edited).unwrap(),
    );
    let reported = coord
        .report(
            &assignment,
            "result",
            Some("shipped the local change"),
            Some(SHA),
        )
        .expect("origin report");
    assert_eq!(reported["state"], "result");
    assert_eq!(reported["reported_head_sha"], SHA);
    let calls = script.calls();
    let report = calls
        .iter()
        .rev()
        .find(|call| call.is_suffix("/report"))
        .unwrap();
    assert_eq!(report.body["claim_id"], origin_claim);
    assert_eq!(report.body["expected_generation"], origin_generation);
    assert_eq!(report.body["state"], "result");
    assert_eq!(report.body["reported_head_sha"], SHA);
    assert_ne!(
        report.body["claim_id"],
        "hlc_00000000000000000000000000000099"
    );
    let ledger = ledger_file(auth.path());
    let saved = attempt(&ledger, &assignment);
    assert_eq!(saved["claim_id"], "hlc_00000000000000000000000000000099");
    assert_eq!(saved["generation"], 9);
    assert_ne!(saved["state"], "result");
}

#[test]
fn external_reclaim_does_not_rewrite_the_first_kickoff_binding() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), THEM);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let first = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("first claim");
    let first_message = actor_message(&first);
    let first_claim = first["claimed"][0]["claim_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut other_client = script.clone();
    let moved = other_client
        .send(
            "POST",
            &format!("{}/{assignment}/reassign", base()),
            Some(&json!({
                "version": VERSION,
                "organization_id": ORG,
                "expected_generation": first["claimed"][0]["generation"],
                "operation_id": "op_external_move",
                "target": ME
            })),
        )
        .expect("another client reassigns");
    assert_eq!(moved.status, 200);
    let moved = moved.body;
    assert_ne!(moved["generation"], first["claimed"][0]["generation"]);
    assert!(moved["claim_id"].is_null());
    let second = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("second claim");
    let second_message = actor_message(&second);
    let second_claim = second["claimed"][0]["claim_id"].as_str().unwrap();
    assert_ne!(second_message, first_message);
    assert_ne!(second_claim, first_claim);
    let ledger = ledger_file(auth.path());
    assert_eq!(ledger["origins"][&first_message]["claim_id"], first_claim);
    assert_eq!(ledger["origins"][&second_message]["claim_id"], second_claim);
    assert_eq!(attempt(&ledger, &assignment)["claim_id"], second_claim);
    let first_bind = bind_file(auth.path(), &first_message);
    let second_bind = bind_file(auth.path(), &second_message);
    assert_eq!(first_bind["claim_id"], first_claim);
    assert_eq!(first_bind["message_id"], first_message);
    assert_eq!(second_bind["claim_id"], second_claim);
    assert_eq!(
        second_bind["generation"],
        second["claimed"][0]["generation"]
    );
    let stale = coord.report_claimed(
        &assignment,
        &first_claim,
        first["claimed"][0]["generation"].as_u64().unwrap(),
        "result",
        Some("stale worker"),
        Some(SHA),
    );
    assert_eq!(stale.err().expect("stale result").code(), Some("fenced"));
    let (board_state, _, board_claim) = script.row(&assignment);
    assert_eq!(board_state, "running");
    assert_eq!(board_claim.as_deref(), Some(second_claim));
    let ledger = ledger_file(auth.path());
    let saved = attempt(&ledger, &assignment);
    assert_eq!(saved["claim_id"], second_claim);
    assert_eq!(saved["fenced"], false);
    assert_eq!(saved["kickoff_message"], second_message);
    let posted = coord
        .report(
            &assignment,
            "result",
            Some("second claim finished"),
            Some(SHA),
        )
        .expect("current origin");
    assert_eq!(posted["claim_id"], second_claim);
    let calls = script.calls();
    let report = calls
        .iter()
        .rev()
        .find(|call| call.is_suffix("/report"))
        .unwrap();
    assert_eq!(report.body["claim_id"], second_claim);
    assert_eq!(
        bind_file(auth.path(), &first_message)["claim_id"],
        first_claim
    );
}

#[test]
fn external_claim_of_the_replacement_gets_a_new_kickoff() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), THEM);
    let delayed_old_generation = board.rows.clone();
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let first = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("first claim");
    let first_message = actor_message(&first);
    let first_claim = first["claimed"][0]["claim_id"]
        .as_str()
        .unwrap()
        .to_string();
    let old_path = PathBuf::from(first["claimed"][0]["worktree"].as_str().unwrap());
    fs::write(old_path.join("kept.txt"), "old edits\n").expect("old file");
    let client = ledger_file(auth.path())["client_agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut other_client = script.clone();
    let moved = other_client
        .send(
            "POST",
            &format!("{}/{assignment}/reassign", base()),
            Some(&json!({
                "version": VERSION,
                "organization_id": ORG,
                "expected_generation": first["claimed"][0]["generation"],
                "operation_id": "op_external_move",
                "target": ME
            })),
        )
        .expect("another client reassigns");
    assert_eq!(moved.status, 200);
    let claimed = other_client
        .send(
            "POST",
            &format!("{}/{assignment}/claim", base()),
            Some(&json!({
                "version": VERSION,
                "organization_id": ORG,
                "expected_generation": moved.body["generation"],
                "client_agent_id": client
            })),
        )
        .expect("another process claims the replacement");
    assert_eq!(claimed.status, 200);
    let second = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("adopt the claimed replacement");
    let second_message = actor_message(&second);
    let second_claim = second["claimed"][0]["claim_id"].as_str().unwrap();
    assert_ne!(second_message, first_message);
    assert_ne!(second_claim, first_claim);
    assert!(second["claimed"][0]["actor"].get("state").is_none());
    let new_path = PathBuf::from(second["claimed"][0]["worktree"].as_str().unwrap());
    assert_ne!(new_path, old_path);
    assert_eq!(
        fs::read_to_string(old_path.join("kept.txt")).expect("old checkout"),
        "old edits\n"
    );
    let ledger = ledger_file(auth.path());
    assert_eq!(ledger["origins"][&first_message]["claim_id"], first_claim);
    assert_eq!(ledger["origins"][&second_message]["claim_id"], second_claim);
    assert_eq!(
        attempt(&ledger, &assignment)["kickoff_message"],
        second_message
    );
    assert_eq!(
        bind_file(auth.path(), &first_message)["claim_id"],
        first_claim
    );
    assert_eq!(
        bind_file(auth.path(), &second_message)["claim_id"],
        second_claim
    );
    script.0.lock().expect("board").list_snapshot = Some(delayed_old_generation);
    let delayed = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("old generation is refused in the batch");
    assert!(delayed["claimed"].as_array().unwrap().is_empty());
    assert_eq!(delayed["refused"][0]["code"], "fenced");
    script.0.lock().expect("board").list_snapshot = None;
    let current = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("newer claim survives the delayed observation");
    assert_eq!(actor_message(&current), second_message);
    assert_eq!(current["claimed"][0]["actor"]["state"], "already_delivered");
    assert_eq!(
        ledger_file(auth.path())["origins"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn lost_reassign_response_keeps_its_operation_after_the_board_moves() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    board.fail_reassign = true;
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let first = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("first claim");
    let first_message = actor_message(&first);
    let first_generation = first["claimed"][0]["generation"].clone();
    let failed = coord
        .reassign(&assignment, None, true)
        .expect_err("board has not accepted cancel");
    assert_eq!(failed.code(), Some("unavailable"));
    let saved_operation = script
        .calls()
        .iter()
        .rev()
        .find(|call| call.is_suffix("/reassign"))
        .unwrap()
        .body["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut other_client = script.clone();
    let moved = other_client
        .send(
            "POST",
            &format!("{}/{assignment}/reassign", base()),
            Some(&json!({
                "version": VERSION,
                "organization_id": ORG,
                "expected_generation": first_generation,
                "operation_id": "op_external_move",
                "target": ME
            })),
        )
        .expect("board moved while the local response was lost");
    assert_eq!(moved.status, 200);
    let second = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("replacement claim");
    assert_ne!(actor_message(&second), first_message);
    let replayed = coord
        .reassign(&assignment, None, true)
        .expect_err("original cancel is replayed against its saved generation");
    assert_eq!(replayed.code(), Some("fenced"));
    let replays: Vec<_> = script
        .calls()
        .into_iter()
        .filter(|call| call.is_suffix("/reassign") && call.body["cancel"].as_bool() == Some(true))
        .collect();
    assert_eq!(replays.len(), 2);
    assert_eq!(replays[0].body["operation_id"], saved_operation);
    assert_eq!(replays[1].body["operation_id"], saved_operation);
    assert_eq!(replays[0].body["expected_generation"], first_generation);
    assert_eq!(replays[1].body["expected_generation"], first_generation);
    let ledger = ledger_file(auth.path());
    let saved = attempt(&ledger, &assignment);
    assert_eq!(saved["reassign_operation_id"], saved_operation);
    assert_eq!(saved["reassign_expected_generation"], first_generation);
    assert_eq!(
        ledger["origins"][&first_message]["claim_id"],
        first["claimed"][0]["claim_id"]
    );
    assert_ne!(saved["kickoff_message"], first_message);
}

#[test]
fn another_members_waiting_assignment_is_not_taken() {
    let auth = private_dir();
    let repo = private_dir();
    git(repo.path(), &["init", "-b", "main"]);
    let mut board = Board::empty();
    board.seed("start", "waiting", None, THEM);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    coord.bind_alias("worker").expect("alias");
    let mut deliver = |_: &Kickoff| -> Result<Value> {
        panic!("another member's waiting assignment must not be delivered");
    };
    let error = coord
        .start_here(ISSUE, repo.path(), None, &mut deliver)
        .expect_err("not ours");
    assert_eq!(error.code(), Some("assignment_live"));
    assert!(script.calls().iter().all(|call| call.method == "GET"));
    assert!(!crate::worktree::layout::worktree_dir(repo.path(), "cad1460-work").exists());
}

#[test]
fn delayed_queued_observations_reuse_one_reserved_kickoff() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    board.seed("dispatch", "queued", Some(ME), ME);
    board.list_snapshot = Some(board.rows.clone());
    let script = Script::new(board);
    let mut first = open(&script, auth.path());
    let mut second = open(&script, auth.path());
    let mut delivered = Vec::new();
    let mut deliver = |kickoff: &Kickoff| {
        store_kickoff_binding(auth.path(), "acme", ENDPOINT, kickoff, KICKOFF)?;
        delivered.push(kickoff.message_id.to_string());
        Ok(json!({"message": kickoff.message_id}))
    };
    let left = first
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("first claim");
    let right = second
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("delayed queued observation replays the live claim");
    assert_eq!(actor_message(&left), actor_message(&right));
    assert_eq!(right["claimed"][0]["actor"]["state"], "already_delivered");
    assert_eq!(delivered, [actor_message(&left)]);
    let ledger = ledger_file(auth.path());
    assert_eq!(ledger["origins"].as_object().unwrap().len(), 1);
    assert_eq!(
        bind_file(auth.path(), &actor_message(&left))["claim_id"],
        left["claimed"][0]["claim_id"]
    );
}

#[test]
fn delayed_running_observation_cannot_start_after_remote_cancellation() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    board.seed("dispatch", "queued", Some(ME), THEM);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let queued = coord
        .list_state(Some("queued"))
        .expect("queued list")
        .pop()
        .expect("queued row");
    coord.claim(&queued).expect("claim before local delivery");
    {
        let mut board = script.0.lock().expect("board");
        board.list_snapshot = Some(board.rows.clone());
        let row = &mut board.rows[0];
        row.generation += 1;
        row.state = "cancelled".to_string();
        row.claim_id = None;
        row.client_agent_id = None;
        row.target = None;
    }
    let mut delivered = false;
    let connected = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut |kickoff| {
            delivered = true;
            Ok(json!({"message": kickoff.message_id}))
        })
        .expect("cancelled claim is refused in the batch");
    assert!(connected["claimed"].as_array().unwrap().is_empty());
    assert_eq!(connected["refused"][0]["code"], "fenced");
    assert!(
        !delivered,
        "a cached running row is not execution authority"
    );
}

#[test]
fn older_queued_snapshot_does_not_erase_the_newer_claim() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliveries = Vec::new();
    let mut deliver = |kickoff: &Kickoff| {
        deliveries.push(kickoff.message_id.to_string());
        Ok(json!({"message": kickoff.message_id}))
    };
    let first = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("first generation");
    let first_message = actor_message(&first);
    coord
        .reassign(&assignment, Some(ME), false)
        .expect("newer generation");
    let second = coord
        .connect(None, None, None, &mut deliver)
        .expect("second generation");
    let second_message = actor_message(&second);
    assert_ne!(second_message, first_message);
    {
        let mut board = script.0.lock().expect("board");
        let mut stale = board.rows[0].clone();
        stale.generation = 1;
        stale.state = "queued".to_string();
        stale.claim_id = None;
        stale.client_agent_id = None;
        board.list_snapshot = Some(vec![stale]);
    }
    let watched = coord
        .connect(None, None, None, &mut deliver)
        .expect("older snapshot is not execution authority");
    assert!(watched["claimed"].as_array().unwrap().is_empty());
    assert_eq!(watched["refused"][0]["code"], "fenced");
    assert_eq!(
        deliveries,
        vec![first_message.clone(), second_message.clone()]
    );
    let ledger = ledger_file(auth.path());
    assert_eq!(ledger["origins"].as_object().unwrap().len(), 2);
    assert_eq!(
        attempt(&ledger, &assignment)["kickoff_message"],
        second_message
    );
    assert_eq!(
        attempt(&ledger, &assignment)["claim_id"],
        second["claimed"][0]["claim_id"]
    );
    assert_eq!(
        attempt(&ledger, &assignment)["generation"],
        second["claimed"][0]["generation"]
    );
    assert_eq!(
        ledger["origins"][&first_message]["claim_id"],
        first["claimed"][0]["claim_id"]
    );
}

#[test]
fn stale_null_claim_keeps_pending_reassign_generation() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    board.fail_reassign = true;
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliveries = 0;
    let mut deliver = |kickoff: &Kickoff| {
        deliveries += 1;
        Ok(json!({"message": kickoff.message_id}))
    };
    let first = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("claim");
    let message = actor_message(&first);
    let generation = first["claimed"][0]["generation"].clone();
    let failed = coord
        .reassign(&assignment, None, true)
        .expect_err("lost cancel acknowledgement");
    assert_eq!(failed.code(), Some("unavailable"));
    let operation = script
        .calls()
        .iter()
        .rev()
        .find(|call| call.is_suffix("/reassign"))
        .unwrap()
        .body["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    {
        let mut board = script.0.lock().expect("board");
        let mut stale = board.rows[0].clone();
        stale.state = "queued".to_string();
        stale.claim_id = None;
        stale.client_agent_id = None;
        board.list_snapshot = Some(vec![stale]);
    }
    let again = coord
        .connect(None, None, None, &mut deliver)
        .expect("null claim snapshot replays the live claim");
    assert_eq!(deliveries, 1);
    assert_eq!(actor_message(&again), message);
    assert_eq!(again["claimed"][0]["actor"]["state"], "already_delivered");
    let ledger = ledger_file(auth.path());
    let saved = attempt(&ledger, &assignment);
    assert_eq!(saved["reassign_operation_id"], operation);
    assert_eq!(saved["reassign_fingerprint"], "cancel");
    assert_eq!(saved["reassign_expected_generation"], generation);
    assert_eq!(saved["kickoff_message"], message);
    assert_eq!(ledger["origins"].as_object().unwrap().len(), 1);
    assert_eq!(
        ledger["origins"][&message]["claim_id"],
        first["claimed"][0]["claim_id"]
    );
}

#[test]
fn stale_running_snapshot_after_terminal_result_does_not_execute() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let deliveries = std::cell::Cell::new(0);
    let mut deliver = |kickoff: &Kickoff| {
        deliveries.set(deliveries.get() + 1);
        Ok(json!({"message": kickoff.message_id}))
    };
    let first = coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("claim");
    let message = actor_message(&first);
    coord
        .report(&assignment, "result", Some("finished locally"), Some(SHA))
        .expect("result");
    assert_eq!(script.row(&assignment).0, "result");
    {
        let mut board = script.0.lock().expect("board");
        let mut running = board.rows[0].clone();
        running.state = "running".to_string();
        board.list_snapshot = Some(vec![running]);
    }
    let watched = coord
        .connect(None, None, None, &mut deliver)
        .expect("terminal board refuses the stale running list");
    assert!(watched["claimed"].as_array().unwrap().is_empty());
    assert_eq!(watched["refused"][0]["code"], "not_claimable");
    assert_eq!(deliveries.get(), 1);
    assert_eq!(script.row(&assignment).0, "result");
    {
        let mut board = script.0.lock().expect("board");
        let running = board.list_snapshot.as_ref().unwrap()[0].clone();
        board.claim_snapshot = Some(running);
    }
    let replayed = coord
        .connect(None, None, None, &mut deliver)
        .expect("stale running claim body is not reopened");
    assert!(replayed["claimed"].as_array().unwrap().is_empty());
    assert_eq!(replayed["refused"][0]["code"], "terminal");
    assert_eq!(deliveries.get(), 1);
    assert_eq!(script.row(&assignment).0, "result");
    let ledger = ledger_file(auth.path());
    assert_eq!(ledger["origins"].as_object().unwrap().len(), 1);
    assert_eq!(ledger["origins"][&message]["settled"], true);
    assert_eq!(attempt(&ledger, &assignment)["state"], "result");
    assert!(attempt(&ledger, &assignment)["kickoff_message"].is_null());
    assert!(attempt(&ledger, &assignment)["kickoff_reserved"].is_null());
}

#[test]
fn paused_generation_cannot_deliver_the_next_claims_kickoff() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut first = open(&script, auth.path());
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    first.pause_after_delivery_check(Arc::new(move || {
        entered_tx.send(()).expect("entered activation hook");
        release_rx
            .lock()
            .expect("release receiver")
            .recv_timeout(Duration::from_secs(10))
            .expect("generation 2 must release the stale activation");
    }));
    let stale_deliveries = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let recorded = Arc::clone(&stale_deliveries);
    let mut deliver_stale = move |kickoff: &Kickoff| {
        recorded
            .lock()
            .expect("deliveries")
            .push((kickoff.message_id.to_string(), kickoff.worktree.to_string()));
        Ok(json!({"message": kickoff.message_id}))
    };
    let repo_for_stale = repo.path().to_path_buf();
    let stale = thread::spawn(move || {
        first.connect(
            Some(&repo_for_stale),
            Some("work"),
            Some("worker"),
            &mut deliver_stale,
        )
    });
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("generation 1 reaches reservation boundary");
    let paused = ledger_file(auth.path());
    let old_checkout = attempt(&paused, &assignment)["worktree"]
        .as_str()
        .expect("gen1 checkout")
        .to_string();
    let old_claim = attempt(&paused, &assignment)["claim_id"].clone();
    assert_eq!(attempt(&paused, &assignment)["generation"], 1);
    assert!(paused["origins"].as_object().unwrap().is_empty());

    let mut second = open(&script, auth.path());
    let moved = second
        .reassign(&assignment, Some(ME), false)
        .expect("reassign to generation 2");
    let mut deliver_current = deliver_to(auth.path().to_path_buf());
    let current = second
        .connect(
            Some(repo.path()),
            Some("work"),
            Some("worker"),
            &mut deliver_current,
        )
        .expect("generation 2 reserves its own kickoff");
    let current_message = actor_message(&current);
    let current_checkout = current["claimed"][0]["worktree"]
        .as_str()
        .expect("gen2 checkout")
        .to_string();
    assert_ne!(current_checkout, old_checkout);
    let reserved = ledger_file(auth.path());
    let origin = reserved["origins"][&current_message].clone();
    assert_eq!(origin["generation"], moved["generation"]);
    assert_eq!(origin["claim_id"], current["claimed"][0]["claim_id"]);
    assert_ne!(origin["claim_id"], old_claim);
    assert_eq!(reserved["origins"].as_object().unwrap().len(), 1);

    release_tx.send(()).expect("release stale activation");
    let stale = stale.join().expect("stale activation").expect("refusal");
    assert!(stale["claimed"].as_array().expect("claimed").is_empty());
    assert_eq!(stale["refused"][0]["assignment_id"], assignment);
    assert!(
        stale_deliveries.lock().expect("deliveries").is_empty(),
        "generation 1 delivered a kickoff"
    );
    assert!(Path::new(&old_checkout).is_dir());
    let kept = ledger_file(auth.path());
    assert_eq!(kept["origins"][&current_message], origin);
    assert_eq!(kept["origins"].as_object().unwrap().len(), 1);
    assert_eq!(
        attempt(&kept, &assignment)["kickoff_reserved"].as_str(),
        Some(current_message.as_str())
    );

    let again = second
        .connect(None, None, None, &mut deliver_current)
        .expect("generation 2 reconnect");
    assert_eq!(actor_message(&again), current_message);
    assert_eq!(again["claimed"][0]["actor"]["state"], "already_delivered");
    assert_eq!(again["claimed"][0]["worktree"], current_checkout);
    let reused = ledger_file(auth.path());
    assert_eq!(reused["origins"][&current_message], origin);
}

#[test]
fn reclaim_checks_out_a_new_lane_and_keeps_the_old_file() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let first = coord
        .connect(
            Some(repo.path()),
            Some("work"),
            Some("worker"),
            &mut deliver,
        )
        .expect("first claim");
    let old_path = PathBuf::from(first["claimed"][0]["worktree"].as_str().unwrap());
    let old_lane = first["claimed"][0]["lane"].as_str().unwrap().to_string();
    fs::write(old_path.join("kept.txt"), "old edits\n").expect("old file");
    let moved = coord
        .reassign(&assignment, Some(ME), false)
        .expect("reassign");
    assert_ne!(moved["generation"], first["claimed"][0]["generation"]);
    let second = coord
        .connect(
            Some(repo.path()),
            Some("work"),
            Some("worker"),
            &mut deliver,
        )
        .expect("second claim");
    let new_path = PathBuf::from(second["claimed"][0]["worktree"].as_str().unwrap());
    let new_lane = second["claimed"][0]["lane"].as_str().unwrap();
    assert_ne!(new_path, old_path);
    assert_ne!(new_lane, old_lane);
    assert!(new_lane.contains("-work-"));
    assert!(new_lane.contains(&format!(
        "-g{}",
        second["claimed"][0]["generation"].as_u64().unwrap()
    )));
    assert_eq!(
        fs::read_to_string(old_path.join("kept.txt")).expect("preserved"),
        "old edits\n"
    );
    assert!(old_path.is_dir());
    let again = coord
        .connect(None, None, None, &mut deliver)
        .expect("watch");
    assert_eq!(
        again["claimed"][0]["worktree"],
        new_path.display().to_string()
    );
    assert_eq!(again["claimed"][0]["actor"]["state"], "already_delivered");
    let stopped = coord.disconnect().expect("disconnect");
    assert_eq!(stopped["stopped"], false);
    assert_eq!(stopped["reported"], false);
    let (state_now, _, _) = script.row(&assignment);
    assert_eq!(state_now, "running");
}

#[test]
fn changing_the_connected_repo_keeps_an_active_claim_in_its_original_worktree() {
    let auth = private_dir();
    let first_repo = private_dir();
    let next_repo = private_dir();
    checkout(first_repo.path());
    checkout(next_repo.path());
    let mut board = Board::empty();
    board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliver = deliver_to(auth.path().to_path_buf());
    let first = coord
        .connect(Some(first_repo.path()), None, Some("worker"), &mut deliver)
        .expect("first claim");
    let original = PathBuf::from(first["claimed"][0]["worktree"].as_str().unwrap());
    fs::write(original.join("unfinished.txt"), "keep this work\n").expect("local edits");
    let again = coord
        .connect(
            Some(next_repo.path()),
            None,
            Some("another-worker"),
            &mut deliver,
        )
        .expect("reconnect with a different default");
    assert_eq!(
        again["claimed"][0]["worktree"],
        original.display().to_string()
    );
    assert_eq!(
        fs::read_to_string(original.join("unfinished.txt")).expect("original edits"),
        "keep this work\n"
    );
    assert!(!crate::worktree::layout::worktree_dir(
        next_repo.path(),
        first["claimed"][0]["lane"].as_str().unwrap()
    )
    .exists());
}

#[test]
fn blocked_resume_is_running_before_the_actor_can_finish() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut reporter = open(&script, auth.path());
    let mut deliveries = 0;
    let mut deliver = |kickoff: &Kickoff| {
        deliveries += 1;
        if deliveries == 1 {
            return Err(Error::rejected("synthetic lost receipt"));
        }
        assert_eq!(script.row(kickoff.assignment_id).0, "running");
        reporter
            .report(
                kickoff.assignment_id,
                "result",
                Some("finished during handoff"),
                Some(SHA),
            )
            .expect("fast result uses the resumed claim");
        Ok(json!({"message": kickoff.message_id}))
    };
    coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("partial batch records its refused handoff");
    let original_claim = script.row(&assignment).2.expect("blocked claim");
    assert_eq!(script.row(&assignment).0, "blocked");
    coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("resume stays on the original claim");
    let (state_now, _, claim) = script.row(&assignment);
    assert_eq!(state_now, "result");
    assert_eq!(claim.as_deref(), Some(original_claim.as_str()));
    let mut polls = 0;
    let mut watch = |_: &Kickoff| {
        polls += 1;
        Err(Error::rejected("watch must not send another actor message"))
    };
    coord.connect(None, None, None, &mut watch).expect("poll");
    let stopped = coord.disconnect().expect("disconnect");
    assert_eq!(stopped["stopped"], false);
    assert_eq!(stopped["configured"], false);
    assert_eq!(coord.status().expect("status")["configured"], false);
    assert_eq!(
        coord
            .connect(None, None, None, &mut watch)
            .unwrap_err()
            .code(),
        Some("config_required")
    );
    assert_eq!(polls, 0);
    assert_eq!(script.row(&assignment).0, "result");
}

#[test]
fn failed_resume_blocks_the_resumed_claim_again() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), ME);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut attempts = 0;
    let mut deliver = |_: &Kickoff| {
        attempts += 1;
        Err(Error::rejected("actor did not accept the handoff"))
    };
    coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("partial batch records the first refusal");
    let original_claim = script.row(&assignment).2.expect("blocked claim");
    assert_eq!(script.row(&assignment).0, "blocked");
    coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("partial batch records the resume refusal");
    assert_eq!(attempts, 2);
    let (state_now, _, claim) = script.row(&assignment);
    assert_eq!(state_now, "blocked");
    assert_eq!(claim.as_deref(), Some(original_claim.as_str()));
}

fn bind_file(dir: &Path, message: &str) -> Value {
    let bytes = fs::read(
        dir.join("hosted-assignments")
            .join(format!("{message}.bind")),
    )
    .expect("bind");
    serde_json::from_slice(&bytes).expect("bind json")
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .expect("ledger write");
    file.write_all(bytes).expect("ledger bytes");
}

impl CoordHttp for Script {
    fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        self.0
            .lock()
            .expect("board")
            .handle(method, path, body.cloned())
    }
}

#[test]
fn distinct_assignments_with_the_same_uuid_tail_do_not_share_a_lane() {
    let first = super::bounded_lane(
        "SYN-1",
        Some("work"),
        "hla_00000000-0000-4000-8000-000000000001",
        1,
    )
    .unwrap();
    let second = super::bounded_lane(
        "SYN-1",
        Some("work"),
        "hla_11111111-0000-4000-8000-000000000001",
        1,
    )
    .unwrap();
    assert_ne!(first, second);
}

#[test]
fn terminal_history_does_not_hide_older_queued_teammate_work() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), THEM);
    for index in 0..50 {
        board.seed("dispatch", "cancelled", Some(ME), ME);
        board.rows.last_mut().expect("terminal row").issue_id = format!("DONE-{index}");
    }
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut delivered = Vec::new();
    let mut deliver = |kickoff: &Kickoff| {
        delivered.push(kickoff.assignment_id.to_string());
        Ok(json!({"message": kickoff.message_id}))
    };
    coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("connect");
    assert_eq!(script.row(&assignment).0, "running");
    assert_eq!(delivered, [assignment]);
}

#[test]
fn background_pickup_does_not_resume_a_blocked_actor() {
    let auth = private_dir();
    let repo = private_dir();
    checkout(repo.path());
    let mut board = Board::empty();
    let assignment = board.seed("dispatch", "queued", Some(ME), THEM);
    let script = Script::new(board);
    let mut coord = open(&script, auth.path());
    let mut deliveries = 0;
    let mut deliver = |kickoff: &Kickoff| {
        deliveries += 1;
        Ok(json!({"message": kickoff.message_id}))
    };
    coord
        .connect(Some(repo.path()), None, Some("worker"), &mut deliver)
        .expect("connect");
    coord
        .report(&assignment, "blocked", Some("Waiting for user input"), None)
        .expect("block");
    let before = script.row(&assignment);
    coord
        .pickup(None, false, &mut deliver)
        .expect("background pickup");
    assert_eq!(script.row(&assignment), before);
    assert_eq!(deliveries, 1);
}

#[test]
fn status_reads_a_valid_fifty_result_response_larger_than_sixty_four_kib() {
    use std::io::{Read, Write};
    let auth = private_dir();
    let mut board = Board::empty();
    let text = "x".repeat(4000);
    for _ in 0..50 {
        board.seed("dispatch", "result", Some(ME), ME);
        let row = board.rows.last_mut().expect("result");
        row.text = Some(text.clone());
        row.head = Some(SHA.to_string());
    }
    let payload = json!({
        "version": VERSION,
        "assignments": board.rows.iter().map(Row::json).collect::<Vec<_>>(),
    })
    .to_string();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    listener.set_nonblocking(true).expect("nonblocking");
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("response fixture accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("timeout");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).expect("request");
            assert!(count > 0 && request.len() + count < 8192);
            request.extend_from_slice(&buffer[..count]);
        }
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len()).expect("headers");
        stream.write_all(payload.as_bytes()).expect("body");
    });
    let http = super::http::BoardHttp::from_parts(
        endpoint,
        ORG.to_string(),
        format!("hct_{}", "s".repeat(43)),
        auth.path().to_path_buf(),
        Some(ME.to_string()),
        None,
    );
    let mut coord =
        Coordinator::new(http, ORG.to_string(), auth.path().to_path_buf()).expect("coordinator");
    coord.set_subject(ME);
    let result = coord.status();
    server.join().expect("response fixture");
    let status = result.expect("valid API result batch");
    assert_eq!(
        status["assignments"].as_array().expect("assignments").len(),
        50
    );
    assert_eq!(
        status["assignments"][49]["text"].as_str(),
        Some(text.as_str())
    );
}
