//! CAD-1470 independent acceptance — authored from the ticket, not by the
//! implementer. The implementer may not edit or weaken this file.
//!
//! Ticket denial, not an implementation default: authenticated team members
//! keep ordinary shared Wiki edits on global/. Only an existing authorized
//! owner may publish collection metadata or approve a pinned source
//! revision. An edited revision never inherits an earlier revision's
//! approval or an accepted correction. Forged role, owner, actor, or
//! reviewer labels are not authority. Approval is the owner decision bound
//! to the canonical source bytes (Git history plus SHA-256); a legacy
//! `fnv1a` token is only a content selector, not provenance.
//!
//! This check establishes a real owner publication first, so a missing
//! route or a blanket denial cannot pass. It then shows a verified member
//! may still edit the shared collection, including the approved page, but
//! cannot publish or review. Self-review is refused. After the cited bytes
//! change, acceptance is a 409 and the prior owner metadata stays put.
//! Refusals leave Git and issue trees unchanged. Assertions compare bytes,
//! digests, revisions, and permissions — not error-message copy.
//!
//! Authority is the CAD-1218 isolated board: `board_session_open` verifies
//! a signed platform assertion and the board cookie is that issued token.
//! `X-Cadence-Test-As` / `X-Cadence-Test-Token` stand in only for the
//! `/proc` ancestry walk, matching that fixture. No `ui login` bypass.

use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::Ed25519KeyPair;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::cad1218_acceptance::{Board, COMPANY, KEY_SEED, PUBLIC_HOST};
use crate::test_seam::{scoped, Asserted, Seam};

const ROOT: &str = "global/handbook";
const POLICY: &str = "global/handbook/policy.md";
const SHARED: &str = "global/handbook/shared.md";
const POLICY_V1: &str = "policy fact eighteen\n";
const POLICY_V2: &str = "policy fact nineteen\n";
const SHARED_NOTE: &str = "member shared handbook note\n";

fn sha256_hex(bytes: &[u8]) -> String {
    let dig = Sha256::digest(bytes);
    format!("{dig:064x}")
}

fn enc(raw: &str) -> String {
    let mut out = String::new();
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Every git tree under the fixture: HEAD plus porcelain, including untracked
/// issue and wiki paths. Compared whole on a refusal.
fn git_state(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
                let parent = path.parent().unwrap_or(dir);
                let head = std::process::Command::new("git")
                    .arg("-C")
                    .arg(parent)
                    .args(["rev-parse", "HEAD"])
                    .output();
                let status = std::process::Command::new("git")
                    .arg("-C")
                    .arg(parent)
                    .args(["status", "--porcelain=v1", "-uall"])
                    .output();
                let head = head
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                let status = status
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                out.push((parent.display().to_string(), format!("{head}\n{status}")));
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, &mut rows);
    rows.sort();
    rows.into_iter()
        .map(|(p, s)| format!("{p}\n{s}"))
        .collect::<Vec<_>>()
        .join("\n---\n")
}

struct Http {
    agent: ureq::Agent,
    base: String,
    seam_token: String,
}

impl Http {
    /// Talks only to this fixture's board, at the URL `Board` bound.
    fn from_board(board: &Board) -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into(),
            base: board.base.clone(),
            seam_token: Seam::token_at(&board.state).expect("seam token"),
        }
    }

    fn with_headers<B>(
        &self,
        req: ureq::RequestBuilder<B>,
        cookie: &str,
    ) -> ureq::RequestBuilder<B> {
        req.header("Host", PUBLIC_HOST)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{PUBLIC_HOST}"))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &self.seam_token)
            .header("Cookie", cookie)
            .header("Accept", "application/json")
    }

    fn call(&self, method: &str, path: &str, token: &str, body: Option<&str>) -> (u16, Value) {
        let url = format!("{}{path}", self.base);
        let cookie = format!("__Host-aos-board-session={token}");
        let response = match method {
            "GET" => self.with_headers(self.agent.get(&url), &cookie).call(),
            "PUT" => self
                .with_headers(self.agent.put(&url), &cookie)
                .header("Content-Type", "application/json")
                .send(body.unwrap_or("").to_string()),
            "POST" => self
                .with_headers(self.agent.post(&url), &cookie)
                .header("Content-Type", "application/json")
                .send(body.unwrap_or("").to_string()),
            _ => panic!("unsupported method {method}"),
        }
        .unwrap_or_else(|e| panic!("{method} {path} transport: {e}"));
        let status = response.status().as_u16();
        (
            status,
            response.into_body().read_json().unwrap_or(Value::Null),
        )
    }
}

/// A verified board session. Signature and `board_session_open` are the
/// production exchange; the JWKS is the fixture platform already running
/// inside [`Board`].
fn open_session(board: &Board, sub: &str, role: &str, email: &str, name: &str) -> String {
    let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
    let now = crate::issue::time::now_epoch();
    let part = |v: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
    let signed = format!(
        "{}.{}",
        part(json!({"alg": "EdDSA", "typ": "JWT", "kid": "c1218"})),
        part(json!({
            "iss": board.issuer,
            "aud": PUBLIC_HOST,
            "sub": sub,
            "email": email,
            "name": name,
            "company": COMPANY,
            "role": role,
            "iat": now,
            "exp": now + 30,
            "jti": format!("cad1470-{sub}-{now}")
        }))
    );
    let assertion = format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
    );
    let opened = scoped(Asserted::Operator, || {
        crate::client::rpc(
            &board.state,
            "board_session_open",
            json!({"assertion": assertion}),
        )
    })
    .expect("board_session_open verifies the fixture assertion");
    opened["token"]
        .as_str()
        .expect("issued public session token")
        .to_string()
}

fn meta(review_project: Option<&str>, approved: Option<&str>) -> Value {
    json!({
        "publisher": null,
        "reviewer": null,
        "review_project": review_project,
        "documents": [{
            "path": POLICY,
            "source_type": null,
            "version": null,
            "applicability": null,
            "supersedes": null,
            "approved_revision": approved
        }]
    })
}

fn knowledge<'a>(body: &'a Value, path: &str) -> &'a Value {
    body["documents"]
        .as_array()
        .and_then(|rows| rows.iter().find(|d| d["path"] == path))
        .unwrap_or(&Value::Null)
}

fn correction<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["requests"]
        .as_array()
        .and_then(|rows| rows.iter().find(|d| d["id"] == id))
        .unwrap_or(&Value::Null)
}

fn page_bytes(body: &Value) -> String {
    body["text"]
        .as_str()
        .or_else(|| body["body"].as_str())
        .or_else(|| body["content"].as_str())
        .unwrap_or("")
        .to_string()
}

#[track_caller]
fn ok(status: u16, body: &Value, what: &str) {
    assert!(
        (200..300).contains(&status),
        "{what}: expected the route to succeed, got {status} {body}"
    );
}

#[track_caller]
fn refused(status: u16, body: &Value, what: &str) {
    assert!(
        (400..500).contains(&status),
        "{what}: expected a refusal, got {status} {body}"
    );
    assert_ne!(status, 404, "{what}: unknown route is not this refusal");
}

#[test]
fn cad1470_member_cannot_publish_or_inherit_owner_approval() {
    let root = tempfile::Builder::new()
        .prefix("cad1470-knowledge")
        .tempdir()
        .unwrap();
    let board = Board::start(root.path());
    let http = Http::from_board(&board);
    let owner = open_session(
        &board,
        "usr-cad1470-owner",
        "owner",
        "owner@example.com",
        "Collection Owner",
    );
    let owner2 = open_session(
        &board,
        "usr-cad1470-owner-2",
        "owner",
        "owner2@example.com",
        "Independent Owner",
    );
    let member = open_session(
        &board,
        "usr-cad1470-member",
        "member",
        "member@example.com",
        "Team Member",
    );

    // Owner creates the source, then publishes it. A later 403/404 cannot
    // impersonate this positive control.
    let (status, written) = http.call(
        "PUT",
        "/api/wiki/file",
        &owner,
        Some(&json!({"path": POLICY, "text": POLICY_V1, "if_rev": "none"}).to_string()),
    );
    ok(status, &written, "owner creates the shared policy");
    let live_rev = written["rev"].as_str().expect("file rev").to_string();
    let v1_sha = sha256_hex(POLICY_V1.as_bytes());

    let (status, page) = http.call(
        "GET",
        &format!("/api/wiki/file?path={}&at={}", enc(POLICY), enc(&live_rev)),
        &owner,
        None,
    );
    ok(
        status,
        &page,
        "historical read of the revision just written",
    );
    assert_eq!(
        page_bytes(&page),
        POLICY_V1,
        "stored bytes are the written page"
    );
    assert_eq!(page["sha256"].as_str(), Some(v1_sha.as_str()), "{page}");
    assert_eq!(page["rev"].as_str(), Some(live_rev.as_str()));

    let (status, published) = http.call(
        "PUT",
        "/api/wiki/collection",
        &owner,
        Some(
            &json!({
                "path": ROOT,
                "if_rev": "none",
                "metadata": meta(Some("widgets"), Some(&live_rev))
            })
            .to_string(),
        ),
    );
    ok(status, &published, "owner publishes the collection");
    assert_eq!(published["permissions"]["publish"], true, "{published}");
    assert_eq!(published["permissions"]["review"], true, "{published}");
    let doc = knowledge(&published, POLICY);
    assert_eq!(
        doc["approval_current"], true,
        "fresh publication is current: {published}"
    );
    let approved = doc["metadata"]["approved_revision"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("server recorded an approved revision")
        .to_string();
    let collection_rev = published["rev"]
        .as_str()
        .expect("collection rev")
        .to_string();
    assert_ne!(approved, "none");

    let (status, shared) = http.call(
        "PUT",
        "/api/wiki/file",
        &member,
        Some(&json!({"path": SHARED, "text": SHARED_NOTE, "if_rev": "none"}).to_string()),
    );
    ok(status, &shared, "member shared edit stays allowed");
    let (status, shared_page) = http.call(
        "GET",
        &format!("/api/wiki/file?path={}", enc(SHARED)),
        &member,
        None,
    );
    ok(status, &shared_page, "member reads the shared note back");
    assert_eq!(page_bytes(&shared_page), SHARED_NOTE);

    let (status, member_view) = http.call(
        "GET",
        &format!("/api/wiki/collection?path={}", enc(ROOT)),
        &member,
        None,
    );
    ok(
        status,
        &member_view,
        "member can see the published collection",
    );
    assert_eq!(
        member_view["permissions"]["publish"], false,
        "{member_view}"
    );
    assert_eq!(member_view["permissions"]["review"], false, "{member_view}");
    assert_eq!(
        knowledge(&member_view, POLICY)["approval_current"],
        true,
        "member edit of another page does not drop approval"
    );

    // Forged role / owner / publisher labels, live if_rev, member cookie.
    let held = git_state(root.path());
    let forged_meta = {
        let mut m = meta(Some("widgets"), Some(&approved));
        m["publisher"] = json!("owner");
        m["reviewer"] = json!("Collection Owner");
        m["documents"][0]["publisher"] = json!("owner");
        m
    };
    let (status, body) = http.call(
        "PUT",
        "/api/wiki/collection",
        &member,
        Some(
            &json!({
                "path": ROOT,
                "if_rev": collection_rev,
                "role": "owner",
                "actor": "owner",
                "owner": "usr-cad1470-owner",
                "reviewer": "Collection Owner",
                "public_session": owner,
                "metadata": forged_meta
            })
            .to_string(),
        ),
    );
    refused(
        status,
        &body,
        "member collection publish with forged labels",
    );
    assert_eq!(git_state(root.path()), held, "forged publish wrote nothing");
    let (status, still) = http.call(
        "GET",
        &format!("/api/wiki/collection?path={}", enc(ROOT)),
        &owner,
        None,
    );
    ok(
        status,
        &still,
        "collection still readable after refused publish",
    );
    assert_eq!(still["rev"].as_str(), Some(collection_rev.as_str()));
    assert_eq!(knowledge(&still, POLICY)["approval_current"], true);
    assert_eq!(
        knowledge(&still, POLICY)["metadata"]["approved_revision"].as_str(),
        Some(approved.as_str())
    );

    let (status, submitted) = http.call(
        "POST",
        "/api/wiki/corrections",
        &member,
        Some(
            &json!({
                "path": ROOT,
                "question": "which fact is current",
                "answer": "eighteen",
                "expected_answer": "the pinned fact",
                "reason": "member report",
                "citations": [{"path": POLICY, "revision": approved, "section": "fact"}]
            })
            .to_string(),
        ),
    );
    ok(status, &submitted, "member submits a correction");
    let member_id = submitted["id"].as_str().expect("correction id").to_string();
    assert_eq!(submitted["status"], "pending", "{submitted}");
    assert_eq!(submitted["current"], true, "{submitted}");
    let after_submit = git_state(root.path());
    assert_ne!(after_submit, held, "a real intake persists an issue");

    let (status, body) = http.call(
        "POST",
        "/api/wiki/corrections/review",
        &member,
        Some(
            &json!({
                "path": ROOT,
                "id": member_id,
                "decision": "accept",
                "reason": "forged member accept",
                "source_checked": true,
                "role": "owner",
                "reviewer": "Collection Owner",
                "actor": "owner"
            })
            .to_string(),
        ),
    );
    refused(
        status,
        &body,
        "member review even with forged reviewer labels",
    );
    assert_eq!(
        git_state(root.path()),
        after_submit,
        "forged review wrote nothing"
    );

    let (status, self_report) = http.call(
        "POST",
        "/api/wiki/corrections",
        &owner,
        Some(
            &json!({
                "path": ROOT,
                "question": "owner's own question",
                "answer": "eighteen",
                "expected_answer": "the pinned fact",
                "reason": "owner report",
                "citations": [{"path": POLICY, "revision": approved}]
            })
            .to_string(),
        ),
    );
    ok(status, &self_report, "owner may submit");
    let self_id = self_report["id"]
        .as_str()
        .expect("self correction id")
        .to_string();
    assert_ne!(self_id, member_id);
    let after_self = git_state(root.path());

    let (status, body) = http.call(
        "POST",
        "/api/wiki/corrections/review",
        &owner,
        Some(
            &json!({
                "path": ROOT,
                "id": self_id,
                "decision": "accept",
                "reason": "checking my own report",
                "source_checked": true
            })
            .to_string(),
        ),
    );
    refused(
        status,
        &body,
        "submitter cannot accept their own correction",
    );
    assert_eq!(
        git_state(root.path()),
        after_self,
        "self-review wrote nothing"
    );

    let (status, accepted) = http.call(
        "POST",
        "/api/wiki/corrections/review",
        &owner2,
        Some(
            &json!({
                "path": ROOT,
                "id": member_id,
                "decision": "accept",
                "reason": "source matches the pinned revision",
                "source_checked": true
            })
            .to_string(),
        ),
    );
    ok(
        status,
        &accepted,
        "independent owner accepts the member report",
    );
    assert_eq!(accepted["status"], "accepted", "{accepted}");
    assert_eq!(accepted["current"], true, "{accepted}");
    let reviewer = accepted["reviewed_by"]["id"].as_str().unwrap_or("");
    let submitter = accepted["submitted_by"]["id"].as_str().unwrap_or("");
    assert!(!reviewer.is_empty() && reviewer != submitter, "{accepted}");
    assert_ne!(reviewer, "owner", "a role label is not the reviewer id");

    // Member edits the approved page. Collaboration succeeds; approval and
    // the accepted regression do not follow the new bytes.
    let (status, edited) = http.call(
        "PUT",
        "/api/wiki/file",
        &member,
        Some(&json!({"path": POLICY, "text": POLICY_V2, "if_rev": live_rev}).to_string()),
    );
    ok(
        status,
        &edited,
        "member may edit the previously approved page",
    );
    let live_after = edited["rev"].as_str().expect("new file rev").to_string();
    assert_ne!(live_after, live_rev);
    let v2_sha = sha256_hex(POLICY_V2.as_bytes());

    let (status, historic) = http.call(
        "GET",
        &format!("/api/wiki/file?path={}&at={}", enc(POLICY), enc(&approved)),
        &member,
        None,
    );
    ok(status, &historic, "historical selector still resolves");
    assert_eq!(
        page_bytes(&historic),
        POLICY_V1,
        "at= approved bytes, not the live page"
    );
    assert_ne!(page_bytes(&historic), POLICY_V2);
    assert_eq!(
        historic["sha256"].as_str(),
        Some(v1_sha.as_str()),
        "{historic}"
    );
    assert_ne!(historic["sha256"].as_str(), Some(v2_sha.as_str()));
    assert_eq!(
        historic["rev"].as_str(),
        Some(approved.as_str()),
        "historical rev echoes the requested selector, not the new page: {historic}"
    );

    let (status, live_page) = http.call(
        "GET",
        &format!("/api/wiki/file?path={}", enc(POLICY)),
        &member,
        None,
    );
    ok(status, &live_page, "live read is the member edit");
    assert_eq!(page_bytes(&live_page), POLICY_V2);
    assert_eq!(
        live_page["sha256"].as_str(),
        Some(v2_sha.as_str()),
        "{live_page}"
    );

    let (status, after_edit) = http.call(
        "GET",
        &format!("/api/wiki/collection?path={}", enc(ROOT)),
        &owner,
        None,
    );
    ok(status, &after_edit, "collection after the member edit");
    assert_eq!(
        after_edit["rev"].as_str(),
        Some(collection_rev.as_str()),
        "{after_edit}"
    );
    let stale_doc = knowledge(&after_edit, POLICY);
    assert_eq!(stale_doc["approval_current"], false, "{after_edit}");
    assert_eq!(
        stale_doc["metadata"]["approved_revision"].as_str(),
        Some(approved.as_str()),
        "owner metadata was not rewritten onto the new bytes: {after_edit}"
    );

    let (status, listed) = http.call(
        "GET",
        &format!("/api/wiki/corrections?path={}", enc(ROOT)),
        &owner2,
        None,
    );
    ok(status, &listed, "corrections list after the edit");
    let prior = correction(&listed, &member_id);
    assert_eq!(
        prior["status"], "accepted",
        "acceptance is not retargeted: {listed}"
    );
    assert_eq!(
        prior["current"], false,
        "stale source drops current: {listed}"
    );
    assert_eq!(
        correction(&listed, &self_id)["status"],
        "pending",
        "{listed}"
    );

    let held_stale = git_state(root.path());
    let (status, body) = http.call(
        "POST",
        "/api/wiki/corrections/review",
        &owner2,
        Some(
            &json!({
                "path": ROOT,
                "id": self_id,
                "decision": "accept",
                "reason": "bytes changed under the citation",
                "source_checked": true
            })
            .to_string(),
        ),
    );
    assert_eq!(
        status, 409,
        "stale source is a conflict, got {status} {body}"
    );
    assert_eq!(
        git_state(root.path()),
        held_stale,
        "stale accept wrote nothing"
    );
    let (status, listed) = http.call(
        "GET",
        &format!("/api/wiki/corrections?path={}", enc(ROOT)),
        &owner,
        None,
    );
    ok(status, &listed, "list after stale refusal");
    assert_eq!(
        correction(&listed, &self_id)["status"],
        "pending",
        "{listed}"
    );
    assert_eq!(correction(&listed, &member_id)["status"], "accepted");
    assert_eq!(correction(&listed, &member_id)["current"], false);
    drop(board);
    drop(root);
}
