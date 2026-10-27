//! CAD-1339 independent acceptance of the renewal response guard.
//!
//! The response shape below is checked against the AOS-205 contract and
//! issuer implementation in the AOS-205 worktree (the Zod exchange-response
//! schema plus `apps/api/src/hosted-cadence/cli-session.ts::accessResponse`).
//! These tests exercise the real CLI validator and local credential writer;
//! they do not claim to exercise issuer HTTP/D1 behavior.
use super::*;
use crate::remote_enrollment::AccessIngress;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const ORG: &str = "acme";
const ORG_ID: &str = "ws_real";
const SUBJECT: &str = "user_owner";
const ISSUER: &str = "https://api-v2.agenticos.hk";
const SESSION_ID: &str = "hcs_12345678-1234-4234-8234-123456789abc";
const OLD_REFRESH: &str = "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const NEW_REFRESH: &str = "hcr_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const OLD_ACCESS: &str = "hct_CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
const NEW_ACCESS: &str = "hct_DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD";

fn fixture() -> (tempfile::TempDir, RemoteTarget, Credential, PendingRenewal) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: "https://acme.cadencecloud.app".into(),
        org_id: ORG_ID.into(),
    };
    let at = now().unwrap();
    let credential = Credential {
        organization_id: ORG_ID.into(),
        organization_slug: Some(ORG.into()),
        audience: target.endpoint.clone(),
        issuer: Some(ISSUER.into()),
        subject_id: Some(SUBJECT.into()),
        capabilities: CAPABILITIES.iter().map(|s| (*s).to_owned()).collect(),
        access_token: OLD_ACCESS.into(),
        expires_at: at.saturating_sub(1),
        cli_session: Some(CliSession {
            session_id: SESSION_ID.into(),
            refresh_token: OLD_REFRESH.into(),
            generation: 1,
            device_label: "cadence CLI".into(),
            idle_expires_at: at + 1000,
            absolute_expires_at: at + 10_000,
        }),
        pending: None,
    };
    let pending = PendingRenewal {
        operation_id: format!("op_{}", "e".repeat(43)),
        expected_generation: 1,
    };
    (root, target, credential, pending)
}

/// A current AOS-205 renewal response: the exchange object is the same strict
/// shape returned by `hostedCadenceExchangeResponseFor`, with the renewable
/// session metadata appended by the issuer route.
fn issuer_response(prior: &Credential, pending: &PendingRenewal, target: &RemoteTarget) -> Value {
    let at = now().unwrap();
    let session = prior.cli_session.as_ref().unwrap();
    json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": prior.organization_id,
        "organization_slug": target.org,
        "audience": target.endpoint,
        "principal": {
            "kind": "user",
            "subject_id": prior.subject_id,
            "current_role": "owner"
        },
        "capabilities": prior.capabilities,
        "credential": {
            "credential_id": "hcp_12345678-1234-4234-8234-123456789abc",
            "access_token": NEW_ACCESS,
            "token_type": "Bearer",
            "issued_at": at,
            "expires_at": at + 300,
            "renewal": "reexchange"
        },
        "cli_session": {
            "session_id": session.session_id,
            "refresh_token": NEW_REFRESH,
            "generation": pending.expected_generation + 1,
            "device_label": session.device_label,
            "idle_expires_at": (at + 1000).min(session.absolute_expires_at),
            "absolute_expires_at": session.absolute_expires_at
        }
    })
}

#[test]
fn forged_renewal_response_is_refused_before_credential_replacement() {
    let (root, target, mut prior, pending) = fixture();
    prior.pending = Some(pending.clone());
    save_credential(root.path(), ORG, &prior).unwrap();
    let path = root.path().join(format!("cli-{ORG}.json"));
    let before = std::fs::read(&path).unwrap();

    // Subject mismatch is a real immutable-identity check in the renewal
    // guard. This is not a hand-written imitation of the guard's outcome.
    let mut forged = issuer_response(&prior, &pending, &target);
    forged["principal"]["subject_id"] = json!("user_attacker");
    assert!(validate_renewal_response(&forged, &prior, &target, &pending).is_err());

    // The production renewal path invokes this guard before decoding or
    // saving response fields. Confirm the persisted authority remains byte
    // for byte unchanged when that guard refuses the response.
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let still_stored = load_credential(root.path(), &target).unwrap();
    assert_eq!(still_stored.access_token, OLD_ACCESS);
    assert_eq!(still_stored.cli_session.unwrap().refresh_token, OLD_REFRESH);
}

#[test]
fn member_renewal_keeps_the_saved_capabilities() {
    let (_root, target, prior, pending) = fixture();
    let mut response = issuer_response(&prior, &pending, &target);
    response["principal"]["current_role"] = json!("member");
    validate_renewal_response(&response, &prior, &target, &pending)
        .expect("member renewal must not be treated as an identity change");
    assert_eq!(response["capabilities"], json!(prior.capabilities));

    response["principal"]["current_role"] = json!("admin");
    assert!(validate_renewal_response(&response, &prior, &target, &pending).is_err());
}

#[test]
fn current_issuer_response_passes_identity_and_rotation_guard() {
    let (_root, target, prior, pending) = fixture();
    let response = issuer_response(&prior, &pending, &target);
    validate_renewal_response(&response, &prior, &target, &pending)
        .expect("AOS-205 response must satisfy the CLI's actual guard");

    let next: CliSession = serde_json::from_value(response["cli_session"].clone()).unwrap();
    assert_eq!(next.session_id, SESSION_ID);
    assert_eq!(next.generation, 2);
    assert_ne!(next.refresh_token, OLD_REFRESH);
    assert!(response["credential"]["expires_at"].as_u64().unwrap() > now().unwrap());
}

/// CAD-1434 renewal fault matrix, driven through the REAL `renewed_credential_with`
/// state machine (lock, load, due check, pending persist, validate, save) and the
/// REAL checked issuer POST (`cli_session_post_for_test`: HTTPS origin, private
/// dir, pinned issuer, access ingress, route allowlist, deadline, redirect and
/// size guards, JSON parse, access-ingress re-confirm). Only the bytes on the
/// wire come from the synthetic `send` closure, which is a closure argument and
/// is never reachable from config, the environment, or a global.
mod matrix {
    use super::*;
    use std::cell::RefCell;

    /// The synthetic wire send: the same argument list as the real send. It
    /// returns the same triple as production: status, body bytes, and the
    /// correlation the production header validator derives from header values.
    type WireSend<'a> = Box<
        dyn FnOnce(
                &str,
                &str,
                Option<&AccessIngress>,
                &Value,
                Duration,
            ) -> Result<(
                u16,
                Vec<u8>,
                Option<crate::remote_enrollment::IssuerCorrelation>,
            )> + 'a,
    >;

    /// One scripted issuer reply. `hop` and `request_id` are the raw header
    /// values the fake issuer sends (`None` = header absent). They are passed
    /// through the production validator, not a copy of it.
    #[derive(Clone)]
    struct Reply {
        status: u16,
        body: Vec<u8>,
        hop: Option<String>,
        request_id: Option<String>,
    }

    fn reply(status: u16, body: Value) -> Reply {
        Reply {
            status,
            body: serde_json::to_vec(&body).unwrap(),
            hop: None,
            request_id: None,
        }
    }

    /// One scripted issuer answer and the recorded requests. `RefCell` keeps the
    /// fake single-threaded and local to one test, so no test races another.
    struct Wire {
        answers: RefCell<Vec<Reply>>,
        hits: RefCell<Vec<(String, Value)>>,
    }

    impl Wire {
        fn new(answers: Vec<(u16, Value)>) -> Self {
            Self::with(answers.into_iter().map(|(s, b)| reply(s, b)).collect())
        }
        fn with(answers: Vec<Reply>) -> Self {
            Self {
                answers: RefCell::new(answers),
                hits: RefCell::new(Vec::new()),
            }
        }
        fn send(&self) -> WireSend<'_> {
            Box::new(move |url, _bearer, _access, body, _timeout| {
                self.hits.borrow_mut().push((url.to_owned(), body.clone()));
                let mut answers = self.answers.borrow_mut();
                if answers.is_empty() {
                    let unexpected = reply(500, json!({"error": "unexpected_renewal"}));
                    return Ok((unexpected.status, unexpected.body, None));
                }
                let next = answers.remove(0);
                let correlation = crate::remote_enrollment::correlation_from_header_values(
                    next.hop.as_deref(),
                    next.request_id.as_deref(),
                );
                Ok((next.status, next.body, correlation))
            })
        }
        fn posts(&self) -> usize {
            self.hits.borrow().len()
        }
    }

    /// A store whose issuer is the pinned synthetic `https` origin. The pin and
    /// the access ingress are the real files the production checks read.
    fn store(pending: Option<PendingRenewal>) -> (tempfile::TempDir, RemoteTarget, Credential) {
        let (root, target, mut cred, _) = fixture();
        cred.pending = pending;
        std::fs::write(root.path().join("trusted-issuer"), format!("{ISSUER}\n")).unwrap();
        std::fs::set_permissions(
            root.path().join("trusted-issuer"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        save_credential(root.path(), ORG, &cred).unwrap();
        (root, target, cred)
    }

    /// The real renewal path with the checked POST and a synthetic wire.
    fn renew(dir: &Path, target: &RemoteTarget, wire: &Wire) -> Result<Credential> {
        renewed_credential_with(
            dir,
            target,
            Instant::now() + Duration::from_secs(30),
            |dir, issuer, path, bearer, body, deadline| {
                crate::remote_enrollment::cli_session_post_for_test(
                    dir,
                    issuer,
                    path,
                    bearer,
                    body,
                    deadline,
                    wire.send(),
                )
            },
        )
    }

    /// The issuer's validated renewal answer for `prior` at `pending`'s generation.
    fn renewal_ok(prior: &Credential, pending: &PendingRenewal, target: &RemoteTarget) -> Value {
        issuer_response(prior, pending, target)
    }

    #[test]
    fn a_transient_503_keeps_the_saved_login_and_the_same_operation_for_a_later_retry() {
        let (root, target, before) = store(None);
        let wire = Wire::new(vec![
            (503, json!({"error": "authority_unavailable"})),
            (503, json!({"error": "authority_unavailable"})),
        ]);
        let first = renew(root.path(), &target, &wire)
            .err()
            .expect("503 must refuse")
            .to_string();
        assert!(first.contains("saved login is kept"), "{first}");
        assert!(!first.contains("cadence login"), "{first}");
        let kept = load_credential(root.path(), &target).unwrap();
        assert_eq!(
            kept.access_token, before.access_token,
            "access replaced on 503"
        );
        let op = kept
            .pending
            .clone()
            .expect("pending operation dropped on 503")
            .operation_id;
        // Later retry: the same pending operation is resent, not a new one.
        let _ = renew(root.path(), &target, &wire);
        let posts = wire.hits.borrow().clone();
        assert_eq!(posts.len(), 2, "one POST per attempt");
        assert_eq!(
            posts[0].1["operation_id"], posts[1].1["operation_id"],
            "retry changed operation id"
        );
        assert_eq!(posts[1].1["operation_id"], json!(op));
    }

    #[test]
    fn a_terminal_flat_invalid_grant_asks_for_login_and_keeps_the_prior_authority() {
        let (root, target, before) = store(None);
        let wire = Wire::new(vec![(400, json!({"error": "invalid_grant"}))]);
        let err = renew(root.path(), &target, &wire)
            .err()
            .expect("must refuse")
            .to_string();
        assert!(err.contains("cadence login"), "{err}");
        let after = load_credential(root.path(), &target).unwrap();
        assert_eq!(after.access_token, before.access_token);
        assert_eq!(wire.posts(), 1);
    }

    #[test]
    fn a_terminal_flat_invalid_token_asks_for_login_and_keeps_the_prior_authority() {
        let (root, target, before) = store(None);
        let wire = Wire::new(vec![(401, json!({"error": "invalid_token"}))]);
        let err = renew(root.path(), &target, &wire)
            .err()
            .expect("must refuse")
            .to_string();
        assert!(err.contains("cadence login"), "{err}");
        let after = load_credential(root.path(), &target).unwrap();
        assert_eq!(after.access_token, before.access_token);
        assert_eq!(wire.posts(), 1);
    }

    #[test]
    fn a_committed_renewal_whose_response_is_lost_is_recovered_by_replaying_the_same_operation() {
        // The issuer commits generation 1 -> 2 and rotates the refresh token, but
        // the response is lost (crash or socket drop) before the CLI saves it. The
        // CLI therefore still holds generation 1 with the pending operation.
        // Recovery: the retry resends the SAME operation id, and the issuer replays
        // its committed result. Build that exact committed answer for the replay.
        let (root, target, before) = store(None);
        let pending = PendingRenewal {
            operation_id: format!("op_{}", "r".repeat(43)),
            expected_generation: 1,
        };
        let committed = renewal_ok(&before, &pending, &target);
        // First attempt: the wire drops (a transport-level loss surfaces as a
        // refusal with no response body). Second attempt: the issuer replays.
        let wire = Wire::new(vec![
            (503, json!({"error": "authority_unavailable"})),
            (200, committed.clone()),
        ]);
        let _ = renew(root.path(), &target, &wire);
        let pend = load_credential(root.path(), &target).unwrap().pending;
        assert!(pend.is_some(), "pending must survive the lost response");
        let recovered = renew(root.path(), &target, &wire).expect("replayed result must recover");
        let session = recovered.cli_session.expect("renewed session");
        assert_eq!(session.generation, 2, "generation advanced exactly once");
        assert_eq!(
            session.refresh_token,
            committed["cli_session"]["refresh_token"].as_str().unwrap()
        );
        assert!(
            recovered.pending.is_none(),
            "operation not cleared after recovery"
        );
        let posts = wire.hits.borrow().clone();
        assert_eq!(posts.len(), 2);
        assert_eq!(
            posts[0].1["operation_id"], posts[1].1["operation_id"],
            "replay used a new operation"
        );
        assert_eq!(posts[0].1["expected_generation"], json!(1));
        assert_eq!(
            posts[1].1["expected_generation"],
            json!(1),
            "replay sent a new generation"
        );
    }

    #[test]
    fn two_concurrent_renewals_of_one_org_are_serialized_into_one_effective_renewal() {
        // Two real command threads renew the same org at once. The real per-org
        // flock must serialize them: the first renews (generation 1 -> 2); the
        // second, reloading under the lock, finds the session already current and
        // does NOT post again. So exactly one renewal reaches the issuer and the
        // generation advances once.
        use std::sync::{Arc, Mutex};
        let (root, target, _) = store(None);
        let dir = Arc::new(root.path().to_path_buf());
        let posts: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        // The issuer answer for the one allowed renewal, built from the saved
        // prior credential (generation 1). The send is a closure per thread.
        let prior = load_credential(root.path(), &target).unwrap();
        let pending = PendingRenewal {
            operation_id: format!("op_{}", "c".repeat(43)),
            expected_generation: 1,
        };
        let answer = Arc::new(renewal_ok(&prior, &pending, &target));
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let waiting_rx = Arc::new(Mutex::new(waiting_rx));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let dir = dir.clone();
                let target = target.clone();
                let posts = posts.clone();
                let answer = answer.clone();
                let waiting_tx = waiting_tx.clone();
                let waiting_rx = waiting_rx.clone();
                std::thread::spawn(move || {
                    CREDENTIAL_WAITING.with(|observer| *observer.borrow_mut() = Some(waiting_tx));
                    renewed_credential_with(
                        &dir,
                        &target,
                        Instant::now() + Duration::from_secs(30),
                        |dir, issuer, path, bearer, body, deadline| {
                            crate::remote_enrollment::cli_session_post_for_test(
                                dir,
                                issuer,
                                path,
                                bearer,
                                body,
                                deadline,
                                |_url, _bearer, _access, body, _timeout| {
                                    posts.lock().unwrap().push(body.clone());
                                    // Release only after the other renewal actually
                                    // encounters the held credential flock.
                                    waiting_rx
                                        .lock()
                                        .unwrap()
                                        .recv_timeout(Duration::from_secs(5))
                                        .expect("the second renewal never contended");
                                    Ok((200, serde_json::to_vec(&*answer).unwrap(), None))
                                },
                            )
                        },
                    )
                    .map(|c| c.cli_session.map(|s| s.generation))
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let sent = posts.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "two threads posted {} renewals, expected one",
            sent.len()
        );
        let ok: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
        assert!(!ok.is_empty(), "no thread completed a renewal: {results:?}");
        let stored = load_credential(root.path(), &target).unwrap();
        assert_eq!(
            stored.cli_session.unwrap().generation,
            2,
            "generation advanced more than once"
        );
        // The single renewal used one fresh, well-formed operation id, and it is
        // the id the POST carried (no reuse of a value the other thread invented).
        let op = sent[0]["operation_id"].as_str().unwrap();
        assert!(op.starts_with("op_") && op.len() >= 35, "{op}");
        assert_eq!(sent[0]["expected_generation"], json!(1));
    }

    /// The AOS renewal emitter's id shape (`crypto.randomUUID()`): 36 chars.
    const ISSUER_ID: &str = "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b";

    #[test]
    fn a_valid_issuer_hop_and_id_are_shown_on_a_flat_terminal_refusal() {
        // The real issuer answers the flat {error} contract with the emitter's
        // two headers. The terminal remediation is unchanged, and the id is shown
        // so an operator can match it to the issuer's single log line.
        let (root, target, before) = store(None);
        let wire = Wire::with(vec![Reply {
            status: 400,
            body: serde_json::to_vec(&json!({"error": "invalid_grant"})).unwrap(),
            hop: Some("issuer".into()),
            request_id: Some(ISSUER_ID.into()),
        }]);
        let err = renew(root.path(), &target, &wire)
            .err()
            .expect("must refuse")
            .to_string();
        assert!(err.contains("cadence login"), "{err}");
        assert!(
            err.contains(ISSUER_ID),
            "the valid issuer id was not shown: {err}"
        );
        // Authority unchanged; the header is never an authorization.
        let after = load_credential(root.path(), &target).unwrap();
        assert_eq!(after.access_token, before.access_token);
        assert_eq!(wire.posts(), 1);
    }

    #[test]
    fn a_valid_issuer_id_on_a_transient_503_keeps_the_saved_login_unchanged() {
        let (root, target, before) = store(None);
        let wire = Wire::with(vec![Reply {
            status: 503,
            body: serde_json::to_vec(&json!({"error": "authority_unavailable"})).unwrap(),
            hop: Some("issuer".into()),
            request_id: Some(ISSUER_ID.into()),
        }]);
        let err = renew(root.path(), &target, &wire)
            .err()
            .expect("503 must refuse")
            .to_string();
        assert!(err.contains("saved login is kept"), "{err}");
        assert!(!err.contains("cadence login"), "{err}");
        assert!(err.contains(ISSUER_ID), "{err}");
        let kept = load_credential(root.path(), &target).unwrap();
        assert_eq!(kept.access_token, before.access_token);
    }

    #[test]
    fn hostile_overlong_unknown_hop_and_non_uuid_values_are_never_shown() {
        // Each of these must be dropped by the single production validator, so the
        // message carries no id at all. The flat terminal remediation still applies.
        let hostile_hop = "issuer\u{7}ATTACKER-ECHO-do-not-show";
        let overlong = "a".repeat(400);
        let cases: Vec<(Option<&str>, Option<&str>, &str)> = vec![
            (Some("board"), Some(ISSUER_ID), "unknown hop"),
            (Some(hostile_hop), Some(ISSUER_ID), "hostile hop text"),
            (
                Some("issuer"),
                Some("not-a-uuid <script>do-not-show</script>"),
                "non-UUID id",
            ),
            (Some("issuer"), Some(overlong.as_str()), "overlong id"),
            (Some("issuer"), None, "hop without an id"),
            (None, Some(ISSUER_ID), "id without a hop"),
        ];
        for (hop, id, label) in cases {
            let (root, target, _) = store(None);
            let wire = Wire::with(vec![Reply {
                status: 401,
                body: serde_json::to_vec(&json!({"error": "invalid_token"})).unwrap(),
                hop: hop.map(str::to_owned),
                request_id: id.map(str::to_owned),
            }]);
            let err = renew(root.path(), &target, &wire)
                .err()
                .expect("must refuse")
                .to_string();
            assert!(err.contains("cadence login"), "{label}: {err}");
            assert!(
                !err.contains("issuer request"),
                "{label} showed an id: {err}"
            );
            assert!(
                !err.contains("do-not-show"),
                "{label} echoed hostile text: {err}"
            );
            assert!(
                !err.contains("ATTACKER"),
                "{label} echoed hostile text: {err}"
            );
            assert!(
                !err.contains(&overlong),
                "{label} echoed an overlong value: {err}"
            );
            assert!(
                err.len() < 400,
                "{label} message grew with a hostile value: {err}"
            );
        }
    }

    #[test]
    fn a_renewal_refused_before_the_wire_never_reaches_the_send_closure() {
        // Pins are checked before the send. An unpinned issuer is refused with no
        // request sent: the synthetic send must record zero hits.
        let (root, target, _) = store(None);
        std::fs::write(
            root.path().join("trusted-issuer"),
            "https://other.example.test\n",
        )
        .unwrap();
        std::fs::set_permissions(
            root.path().join("trusted-issuer"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let wire = Wire::new(vec![(200, json!({}))]);
        let _ = renew(root.path(), &target, &wire);
        assert_eq!(wire.posts(), 0, "an unpinned issuer reached the wire");
    }

    #[test]
    fn a_plain_http_issuer_is_refused_before_the_wire() {
        // The HTTPS-only production guard runs before the send closure.
        let (root, target, _) = store(None);
        let http = "http://api-v2.agenticos.hk";
        std::fs::write(root.path().join("trusted-issuer"), format!("{http}\n")).unwrap();
        std::fs::set_permissions(
            root.path().join("trusted-issuer"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut cred = load_credential(root.path(), &target).unwrap();
        cred.issuer = Some(http.into());
        save_credential(root.path(), ORG, &cred).unwrap();
        let wire = Wire::new(vec![(200, json!({}))]);
        let err = renew(root.path(), &target, &wire)
            .err()
            .expect("http must refuse")
            .to_string();
        eprintln!("PLAINHTTP_ERR={err}");
        assert!(err.contains("exact HTTPS origin"), "{err}");
        assert_eq!(wire.posts(), 0, "a plain-http issuer reached the wire");
    }

    #[test]
    fn a_pending_renewal_with_a_mismatched_generation_is_refused_before_any_post() {
        let (root, target, mut cred, _) = fixture();
        cred.pending = Some(PendingRenewal {
            operation_id: format!("op_{}", "h".repeat(43)),
            expected_generation: 99,
        });
        save_credential(root.path(), ORG, &cred).unwrap();
        let path = root.path().join(format!("cli-{ORG}.json"));
        let before = std::fs::read(&path).unwrap();
        let wire = Wire::new(vec![(200, json!({}))]);
        let err = renew(root.path(), &target, &wire)
            .err()
            .expect("must refuse")
            .to_string();
        assert!(err.contains("pending CLI renewal is inconsistent"), "{err}");
        assert_eq!(wire.posts(), 0);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "stale pending rewrote the file"
        );
    }

    /// CAD-1853 native operator checks only. Peer authority and hosted
    /// delegation lifetime are deliberately outside this fixture.
    mod cad1853_cloud_operator {
        use super::*;

        const CLOUD: [&str; 3] = ["cloud_agent.write", "bridge.enroll", "cloud_agent.read"];

        fn cloud_store(
            pending: Option<PendingRenewal>,
        ) -> (tempfile::TempDir, RemoteTarget, Credential) {
            let (root, mut target, mut prior) = store(pending);
            prior.capabilities = CLOUD.iter().map(|scope| (*scope).to_owned()).collect();
            target.identity = Some(CliIdentityPin {
                issuer: ISSUER.into(),
                subject_id: SUBJECT.into(),
            });
            save_credential(root.path(), ORG, &prior).unwrap();
            (root, target, prior)
        }

        fn renew_cloud(
            dir: &Path,
            target: &RemoteTarget,
            send: WireSend<'_>,
        ) -> Result<Credential> {
            renewed_cloud_credential_with(
                dir,
                target,
                Instant::now() + Duration::from_secs(30),
                |dir, issuer, path, bearer, body, deadline| {
                    assert_eq!(issuer, ISSUER);
                    assert_eq!(path, "/v1/hosted-cadence/cli-session/renew");
                    assert_eq!(bearer, OLD_REFRESH);
                    assert_eq!(body["version"], "hosted-cadence-cli-session.v1");
                    assert_eq!(body["capabilities"], json!(CLOUD));
                    assert_eq!(body["organization_id"], ORG_ID);
                    assert_eq!(body["organization_slug"], ORG);
                    assert_eq!(body["audience"], target.endpoint);
                    assert_eq!(body["subject_id"], SUBJECT);
                    assert_eq!(body["session_id"], SESSION_ID);
                    assert_eq!(body["device_label"], "cadence CLI");
                    assert_eq!(body["expected_generation"], 1);
                    assert_eq!(body.as_object().unwrap().len(), 10);
                    // This is disk state read before the real guarded issuer
                    // transport boundary, not an in-memory intent assertion.
                    let durable = load_credential(dir, target).unwrap();
                    let pending = durable.pending.expect("intent must precede POST");
                    assert_eq!(body["operation_id"], pending.operation_id);
                    assert_eq!(pending.expected_generation, 1);
                    assert_eq!(durable.access_token, OLD_ACCESS);
                    crate::remote_enrollment::cli_session_post_for_test(
                        dir, issuer, path, bearer, body, deadline, send,
                    )
                },
            )
        }

        #[test]
        fn due_and_expired_cloud_access_renew_without_changing_owner_consent_or_session() {
            for expired in [false, true] {
                let (root, target, mut prior) = cloud_store(None);
                let at = now().unwrap();
                prior.expires_at = if expired { at - 1 } else { at + 30 };
                save_credential(root.path(), ORG, &prior).unwrap();
                let pending = PendingRenewal {
                    operation_id: format!("op_{}", "q".repeat(43)),
                    expected_generation: 1,
                };
                let mut response = issuer_response(&prior, &pending, &target);
                if expired {
                    response["principal"]["current_role"] = json!("member");
                }
                let wire = Wire::new(vec![(200, response.clone())]);
                let next = renew_cloud(root.path(), &target, wire.send())
                    .expect("live original cloud consent must renew due/expired access");
                assert_eq!(wire.posts(), 1, "renew once, never a model retry");
                assert_eq!(next.access_token, NEW_ACCESS);
                assert_eq!(next.issuer, prior.issuer);
                assert_eq!(next.subject_id, prior.subject_id);
                assert_eq!(next.organization_id, prior.organization_id);
                assert_eq!(next.audience, prior.audience);
                assert_eq!(next.capabilities, prior.capabilities);
                assert!(next.pending.is_none());
                let old_session = prior.cli_session.unwrap();
                let session = next.cli_session.unwrap();
                assert_eq!(session.session_id, old_session.session_id);
                assert_eq!(session.device_label, old_session.device_label);
                assert_eq!(session.absolute_expires_at, old_session.absolute_expires_at);
                assert_eq!(session.generation, old_session.generation + 1);
                assert_eq!(session.refresh_token, NEW_REFRESH);
                let saved = load_credential(root.path(), &target).unwrap();
                assert_eq!(saved.access_token, NEW_ACCESS);
                assert_eq!(saved.capabilities, CLOUD);
                assert_eq!(
                    Some(saved.expires_at),
                    response["credential"]["expires_at"].as_u64()
                );
                let no_more_wire = Wire::new(Vec::new());
                let current = renew_cloud(root.path(), &target, no_more_wire.send()).unwrap();
                assert_eq!(current.access_token, NEW_ACCESS);
                assert_eq!(
                    no_more_wire.posts(),
                    0,
                    "fresh access must not rotate again"
                );
            }
        }

        #[test]
        fn cloud_purpose_refuses_invalid_saved_consent_even_when_access_is_not_due() {
            for scopes in [
                json!(["cloud_agent.read", "cloud_agent.write"]),
                json!(["cloud_agent.write", "bridge.enroll"]),
                json!([
                    "cloud_agent.write",
                    "bridge.enroll",
                    "cloud_agent.read",
                    "cli.read",
                    "cli.write"
                ]),
                json!([
                    "cloud_agent.write",
                    "bridge.enroll",
                    "cloud_agent.read",
                    "cloud_agent.read"
                ]),
            ] {
                for fresh in [false, true] {
                    let (root, target, mut prior) = cloud_store(None);
                    prior.capabilities = serde_json::from_value(scopes.clone()).unwrap();
                    prior.expires_at = if fresh {
                        now().unwrap() + 300
                    } else {
                        now().unwrap() - 1
                    };
                    save_credential(root.path(), ORG, &prior).unwrap();
                    let path = root.path().join(format!("cli-{ORG}.json"));
                    let before = std::fs::read(&path).unwrap();
                    let wire = Wire::new(Vec::new());
                    assert!(
                        renew_cloud(root.path(), &target, wire.send()).is_err(),
                        "cloud purpose accepted saved consent {scopes}, fresh={fresh}"
                    );
                    assert_eq!(wire.posts(), 0);
                    assert_eq!(std::fs::read(&path).unwrap(), before);
                }
            }
        }

        #[test]
        fn lost_cloud_renewal_answer_reloads_and_replays_the_exact_durable_intent() {
            let (root, target, prior) = cloud_store(None);
            let path = root.path().join(format!("cli-{ORG}.json"));
            let before: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let first_request = RefCell::new(None);
            let first = renew_cloud(
                root.path(),
                &target,
                Box::new(|_url, _bearer, _access, body, _timeout| {
                    *first_request.borrow_mut() = Some(body.clone());
                    // Only the response bytes are lost at the real checked
                    // send boundary. No upstream D1 commit is claimed here.
                    Err(Error::unknown("synthetic lost renewal acknowledgement"))
                }),
            );
            assert!(first.is_err());
            let sent = first_request
                .into_inner()
                .expect("cloud renewal never reached the checked send boundary");
            let kept = load_credential(root.path(), &target).unwrap();
            let pending = kept.pending.clone().expect("lost reply discarded intent");
            let mut after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let mut authority_before = before;
            after.as_object_mut().unwrap().remove("pending_renewal");
            authority_before
                .as_object_mut()
                .unwrap()
                .remove("pending_renewal");
            assert_eq!(after, authority_before, "loss partially replaced authority");
            assert_eq!(sent["operation_id"], pending.operation_id);
            assert_eq!(sent["expected_generation"], pending.expected_generation);
            let response = issuer_response(&prior, &pending, &target);
            let wire = Wire::new(vec![(200, response)]);
            // A separate invocation reacquires the real lock and reloads disk;
            // no in-memory Credential or returned token is carried across.
            let recovered = renew_cloud(root.path(), &target, wire.send()).unwrap();
            assert_eq!(wire.posts(), 1);
            assert_eq!(wire.hits.borrow()[0].1, sent, "replay changed request body");
            assert_eq!(recovered.access_token, NEW_ACCESS);
            assert!(recovered.pending.is_none());
            assert_eq!(recovered.cli_session.unwrap().generation, 2);
        }

        #[test]
        fn forged_cloud_renewal_answers_cannot_partially_publish_authority() {
            let at = now().unwrap();
            for (pointer, wrong) in [
                ("", json!("not an exchange object")),
                ("/organization_id", json!("ws_attacker")),
                ("/organization_slug", json!("other")),
                ("/audience", json!("https://other.cadencecloud.app")),
                ("/principal/subject_id", json!("user_attacker")),
                ("/principal/kind", json!("service")),
                ("/principal/current_role", json!("admin")),
                (
                    "/capabilities",
                    json!(["cloud_agent.read", "cloud_agent.write"]),
                ),
                (
                    "/capabilities",
                    json!([
                        "cloud_agent.write",
                        "bridge.enroll",
                        "cloud_agent.read",
                        "cli.write"
                    ]),
                ),
                (
                    "/capabilities",
                    json!([
                        "cloud_agent.write",
                        "bridge.enroll",
                        "cloud_agent.read",
                        "cloud_agent.read"
                    ]),
                ),
                (
                    "/cli_session/session_id",
                    json!("hcs_00000000-0000-4000-8000-000000000001"),
                ),
                ("/cli_session/device_label", json!("other device")),
                ("/cli_session/generation", json!(1)),
                ("/cli_session/refresh_token", json!(OLD_REFRESH)),
                ("/cli_session/idle_expires_at", json!(at - 1)),
                ("/cli_session/absolute_expires_at", json!(at + 20_000)),
                ("/credential/issued_at", json!(at + 60)),
                ("/credential/expires_at", json!(at - 1)),
                ("/credential/expires_at", json!(at + 301)),
                ("/credential/access_token", json!("hct_malformed")),
                ("/credential/token_type", json!("Basic")),
            ] {
                let pending = PendingRenewal {
                    operation_id: format!("op_{}", "v".repeat(43)),
                    expected_generation: 1,
                };
                let (root, target, prior) = cloud_store(Some(pending.clone()));
                let path = root.path().join(format!("cli-{ORG}.json"));
                let before = std::fs::read(&path).unwrap();
                let mut response = issuer_response(&prior, &pending, &target);
                response["credential"]["issued_at"] = json!(at);
                *response.pointer_mut(pointer).unwrap() = wrong;
                let wire = Wire::new(vec![(200, response)]);
                assert!(
                    renew_cloud(root.path(), &target, wire.send()).is_err(),
                    "accepted forged response field {pointer}"
                );
                assert_eq!(wire.posts(), 1, "{pointer}: failed before response guard");
                assert_eq!(std::fs::read(&path).unwrap(), before, "{pointer}");
            }
        }

        #[test]
        fn cloud_owner_swap_while_waiting_for_the_lock_is_refused_before_renewal_send() {
            use std::sync::{
                atomic::{AtomicUsize, Ordering},
                Arc,
            };
            let (root, target, prior) = cloud_store(None);
            for replace_issuer in [false, true] {
                save_credential(root.path(), ORG, &prior).unwrap();
                let lock = private_lock(root.path(), ORG, Instant::now() + Duration::from_secs(10))
                    .unwrap();
                let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
                let posts = Arc::new(AtomicUsize::new(0));
                let observed = posts.clone();
                let dir = root.path().to_owned();
                let frozen = target.clone();
                let child = std::thread::spawn(move || {
                    CREDENTIAL_WAITING.with(|observer| *observer.borrow_mut() = Some(waiting_tx));
                    renew_cloud(
                        &dir,
                        &frozen,
                        Box::new(move |_url, _bearer, _access, _body, _timeout| {
                            observed.fetch_add(1, Ordering::SeqCst);
                            Err(Error::unknown("owner replacement must not reach wire"))
                        }),
                    )
                    .map(|_| ())
                });
                let contended = waiting_rx.recv_timeout(Duration::from_secs(5));
                let mut replaced = prior.clone();
                if replace_issuer {
                    replaced.issuer = Some("https://other.example.test".into());
                } else {
                    replaced.subject_id = Some("user_attacker".into());
                }
                // The replacement writer owns the actual source lock. The
                // waiter must reload and compare its original frozen pin.
                save_credential(root.path(), ORG, &replaced).unwrap();
                let path = root.path().join(format!("cli-{ORG}.json"));
                let replacement_bytes = std::fs::read(&path).unwrap();
                drop(lock);
                let result = child.join().unwrap();
                contended.expect("renewal did not contend on the source lock");
                assert!(result.is_err(), "renewal adopted a different human/issuer");
                assert_eq!(posts.load(Ordering::SeqCst), 0);
                assert_eq!(std::fs::read(&path).unwrap(), replacement_bytes);
            }
        }
    }

    #[test]
    fn a_refused_renewal_changes_only_the_pending_record_never_the_authority() {
        // Before the POST the pending operation is persisted (crash-safe). A
        // refusal must leave every authority field byte-for-byte the same: the
        // access token, refresh token, generation and expiry.
        let (root, target, before) = store(None);
        let wire = Wire::new(vec![(401, json!({"error": "invalid_token"}))]);
        let _ = renew(root.path(), &target, &wire);
        let after = load_credential(root.path(), &target).unwrap();
        assert_eq!(
            after.access_token, before.access_token,
            "access token changed"
        );
        assert_eq!(after.expires_at, before.expires_at, "expiry changed");
        let (b, a) = (before.cli_session.unwrap(), after.cli_session.unwrap());
        assert_eq!(
            a.refresh_token, b.refresh_token,
            "refresh token rotated on refusal"
        );
        assert_eq!(a.generation, b.generation, "generation advanced on refusal");
        assert!(
            after.pending.is_some(),
            "the pending operation must be kept for recovery"
        );
    }
}
