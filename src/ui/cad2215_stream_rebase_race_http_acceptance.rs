//! CAD-2215 deterministic signed-board stream rebase-race acceptance.
//! The product implementer must not edit or weaken this check.
//! The per-board test gate controls scheduling only; it grants no authority.

use super::*;

fn start_hooked_board(rig: &mut Rig, hook: Arc<crate::ui::MemberStreamBatchHook>) {
    for port in 3110..=3199u16 {
        if port == rig.board_port || rig.public_host == format!("localhost:{port}") {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(rig.stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![rig.public_host.clone()],
            public: Some(PublicBoard {
                host: rig.public_host.clone(),
                issuer: rig.issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{}/authorize", rig.issuer),
            }),
            member_stream_batch_hook: Some(hook.clone()),
            ..ServeOpts::default()
        };
        let state = rig._dir.path().to_path_buf();
        let pm = rig.pm.clone();
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &opts)));
        match ready
            .recv_timeout(Duration::from_secs(30))
            .expect("hooked board startup did not answer")
        {
            Ok(()) => {
                rig.board_port = port;
                rig._threads.push(thread);
                return;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    panic!("hooked board could not bind");
}

#[test]
fn scoped_stream_reconciles_visible_change_landing_during_revocation_batch() {
    let mut rig = start(&[]);
    seed(&rig);
    // Both projects are readable at stream admission; the later policy write
    // removes the member's access to the secret project.
    let policy = rig.pm.join("secret/project.yaml");
    std::fs::write(&policy, "key: secret\nprefix: SEC\n").unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let release_rx = Arc::new(Mutex::new(release_rx));
    let hook: Arc<crate::ui::MemberStreamBatchHook> = {
        let armed = armed.clone();
        let release_rx = release_rx.clone();
        Arc::new(move |actor, batch| {
            let target = actor == "users/cad2215-race-member"
                && batch.iter().any(|f| f.contains("event: issues\n"))
                && batch
                    .iter()
                    .any(|f| f.contains("CAD2215_RACE_PRIVATE_CHANGE"));
            if target && armed.swap(false, Ordering::SeqCst) {
                entered_tx
                    .send(())
                    .expect("fixture gate observer disappeared");
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(25))
                    .expect("fixture did not release member batch");
            }
        })
    };
    start_hooked_board(&mut rig, hook);
    let member_cookie = login(&rig, "cad2215-race-member", "member");
    let operator_cookie = login(&rig, "cad2215-race-operator", "owner");
    let mut operator = EventStream::open(&rig, &operator_cookie, "race operator");
    let mut member = EventStream::open(&rig, &member_cookie, "race member");

    // Arm only after both real signed streams have initialized. The callback
    // will park this member after it captured the complete batch containing
    // the private issue mutation but before its per-reader filter runs.
    armed.store(true, Ordering::SeqCst);
    std::fs::write(
        &policy,
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    std::fs::write(
        rig.pm.join("secret/SEC-1/issue.md"),
        issue_text("SEC-1", "CAD2215_RACE_PRIVATE_CHANGE"),
    )
    .unwrap();
    entered_rx
        .recv_timeout(Duration::from_secs(25))
        .expect("member did not capture revocation batch");
    let private_control = operator.through("CAD2215_RACE_PRIVATE_CHANGE");
    assert!(
        private_control.contains("SEC-1"),
        "operator did not observe private issue change"
    );

    // The member is still parked. The operator observation proves this
    // later readable edit reached a later watcher batch before release.
    std::fs::write(
        rig.pm.join("visible/VIS-1/issue.md"),
        issue_text("VIS-1", "CAD2215_RACE_VISIBLE_AFTER_CAPTURE"),
    )
    .unwrap();
    let visible_control = operator.through("CAD2215_RACE_VISIBLE_AFTER_CAPTURE");
    assert!(
        visible_control.contains("VIS-1"),
        "operator did not observe later readable issue change"
    );
    release_tx.send(()).expect("member gate disappeared");
    let frames = member.through("CAD2215_RACE_VISIBLE_AFTER_CAPTURE");
    assert!(
        frames.contains("VIS-1"),
        "member failed to receive later readable issue entity: {frames}"
    );
    assert!(
        !frames.contains("SEC-1"),
        "member received revoked private issue identifier: {frames}"
    );
    assert!(
        !frames.contains("CAD2215_RACE_PRIVATE_CHANGE"),
        "member received revoked private title: {frames}"
    );
}
