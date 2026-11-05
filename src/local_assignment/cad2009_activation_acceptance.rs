// Independently authored CAD-2009 acceptance for effectful activation cleanup.
// Included inside cad2009_assignment_acceptance to reuse its bounded HTTP parser.
// No daemon, real credentials, live issuer, sleep-based race, or payload execution.
struct ActivationFixture {
    endpoint: String,
    done: UnixStream,
    worker: std::thread::JoinHandle<(Vec<String>, Value)>,
}

impl ActivationFixture {
    fn start(
        answer: Answer,
        cancel_after_report: bool,
        active: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (done, completion) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut state = original();
            state["state"] = json!("queued");
            state["claim_id"] = Value::Null;
            state["client_agent_id"] = Value::Null;
            let mut posts = Vec::new();
            let mut requests = 0;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "activation fixture deadline elapsed");
                let mut fds = [
                    libc::pollfd {
                        fd: listener.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: completion.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // SAFETY: both live descriptors remain owned by this worker.
                let ready = unsafe {
                    libc::poll(
                        fds.as_mut_ptr(),
                        2,
                        remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                    )
                };
                if ready < 0 {
                    assert_eq!(
                        std::io::Error::last_os_error().kind(),
                        std::io::ErrorKind::Interrupted
                    );
                    continue;
                }
                assert!(ready > 0, "activation fixture did not terminate");
                // Observe queued requests before completion so a replay cannot hide.
                if fds[0].revents & libc::POLLIN != 0 {
                    let (stream, _) = listener.accept().unwrap();
                    let req = request(&stream);
                    requests += 1;
                    assert!(requests <= 12, "unexpected replay/read loop");
                    let (route, query) = req
                        .path
                        .split_once('?')
                        .map_or((req.path.as_str(), None), |(p, q)| (p, Some(q)));
                    if req.method == "GET" {
                        assert_eq!(route, assignment_prefix(ORG));
                        assert!(req.body.is_empty());
                        let wanted = query.map(|q| q.strip_prefix("state=").unwrap());
                        let rows = if wanted.is_none_or(|s| state["state"] == s) {
                            vec![state.clone()]
                        } else {
                            Vec::new()
                        };
                        let bytes = serde_json::to_vec(&json!({
                            "version": VERSION, "assignments": rows,
                        }))
                        .unwrap();
                        response(&stream, 200, &bytes, bytes.len());
                    } else {
                        assert_eq!(req.method, "POST");
                        assert!(query.is_none());
                        let body: Value = serde_json::from_slice(&req.body).unwrap();
                        assert_eq!(body["organization_id"], ORG);
                        assert_eq!(body["version"], VERSION);
                        assert_eq!(body["expected_generation"], 1);
                        if route == format!("{}/{ID}/claim", assignment_prefix(ORG)) {
                            assert!(posts.is_empty(), "claim was replayed");
                            posts.push("claim".to_string());
                            assert_eq!(state["state"], "queued");
                            let client = body["client_agent_id"].as_str().unwrap();
                            assert!(!client.is_empty());
                            state["state"] = json!("running");
                            state["claim_id"] = json!(CLAIM);
                            state["client_agent_id"] = json!(client);
                            let bytes = serde_json::to_vec(&state).unwrap();
                            response(&stream, 200, &bytes, bytes.len());
                        } else {
                            assert_eq!(route, format!("{}/{ID}/report", assignment_prefix(ORG)));
                            assert_eq!(posts, vec!["claim"], "blocked report was replayed");
                            posts.push("report".to_string());
                            assert_eq!(body["claim_id"], CLAIM);
                            assert_eq!(body["state"], "blocked");
                            assert!(body["text"]
                                .as_str()
                                .unwrap()
                                .contains("local lane path is a symlink"));
                            // This is the producer's durable effect, before its lost answer.
                            state["state"] = json!("blocked");
                            state["text"] = body["text"].clone();
                            if cancel_after_report {
                                // A real binding can stop while this report is in flight.
                                // No artificial ensure_active call counter is involved.
                                active.store(false, std::sync::atomic::Ordering::SeqCst);
                            }
                            match answer {
                                Answer::Lost => {}
                                Answer::InvalidJson => response(&stream, 200, b"{", 1),
                                Answer::Truncated => response(&stream, 200, b"{", 100),
                                _ => panic!("unsupported cleanup acceptance response"),
                            }
                        }
                    }
                    let _ = stream.shutdown(Shutdown::Both);
                } else if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                    return (posts, state);
                } else {
                    panic!("unexpected activation fixture poll events");
                }
            }
        });
        Self {
            endpoint,
            done,
            worker,
        }
    }

    fn finish(mut self) -> (Vec<String>, Value) {
        self.done.write_all(b"done").unwrap();
        self.worker.join().expect("bounded activation producer")
    }
}

#[test]
fn cad2009_activation_cleanup_retains_unknown_and_original_local_failure() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for cancel_after_report in [false, true] {
        for answer in [Answer::Lost, Answer::InvalidJson, Answer::Truncated] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            let authority = root.path().join("authority");
            let protected = root.path().join("protected");
            fs::create_dir(&repo).unwrap();
            fs::create_dir(&authority).unwrap();
            fs::create_dir(&protected).unwrap();
            fs::set_permissions(&authority, fs::Permissions::from_mode(0o700)).unwrap();
            let mut init = Command::new("git");
            init.args(["init", "--quiet"]).arg(&repo);
            assert!(crate::reaper::status(&mut init).unwrap().success());
            let lane = bounded_lane("SYN-1", Some("uncertainty"), ID, 1).unwrap();
            let lane_path = layout::worktree_dir(&repo, &lane);
            fs::create_dir_all(lane_path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&protected, &lane_path).unwrap();
            let active = Arc::new(AtomicBool::new(true));
            let fixture = ActivationFixture::start(answer, cancel_after_report, active.clone());
            let transport = http::BoardHttp::from_parts(
                fixture.endpoint.clone(),
                ORG.into(),
                format!("hct_{}", "a".repeat(43)),
                authority.clone(),
                Some("user_target".into()),
                Some("https://issuer.board.localhost".into()),
            );
            let mut coord = Coordinator::open(
                transport,
                ORG.into(),
                authority,
                Some("user_target".into()),
                Some("https://issuer.board.localhost".into()),
                fixture.endpoint.clone(),
            )
            .unwrap();
            coord.bind_alias("acceptance-actor").unwrap();
            coord.delivery_guard = Some(Arc::new(move || active.load(Ordering::SeqCst)));
            let mut deliveries = 0;
            let outcome = coord.start_here(
                "SYN-1",
                Some(&repo),
                Some("uncertainty"),
                None,
                &mut |_| {
                    deliveries += 1;
                    Err(Error::internal("delivery must not run after lane refusal"))
                },
            );
            // An explicit read reconciles producer state; never repeat the mutation.
            let readback = coord.status();
            let (posts, committed) = fixture.finish();
            assert_eq!(
                posts,
                vec!["claim", "report"],
                "one claim and one cleanup report"
            );
            assert_eq!(deliveries, 0, "no actor was handed the refused lane");
            assert_eq!(committed["state"], "blocked");
            assert_eq!(committed["claim_id"], CLAIM);
            assert_eq!(committed["generation"], 1);
            let rows = readback.unwrap()["assignments"].as_array().unwrap().clone();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["state"], "blocked");
            assert_eq!(rows[0]["claim_id"], CLAIM);
            assert!(fs::symlink_metadata(&lane_path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read_dir(&protected).unwrap().count(), 0);
            let error = outcome.expect_err("local activation and report were not confirmed");
            assert_eq!(
                error.kind(),
                "unknown",
                "{answer:?}, cancelled={cancel_after_report}: {error}"
            );
            let message = error.to_string();
            for required in [
                ORG,
                ID,
                CLAIM,
                "expected_generation=1",
                "local lane path is a symlink",
            ] {
                assert!(
                    message.contains(required),
                    "lost context {required:?}: {message}"
                );
            }
            let lower = message.to_ascii_lowercase();
            assert!(
                lower.contains("inspect") || lower.contains("reconcil"),
                "{message}"
            );
            assert!(
                lower.contains("retry") || lower.contains("replay"),
                "{message}"
            );
            assert!(!lower.contains("nothing was recorded"), "{message}");
        }
    }
}
