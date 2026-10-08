//! CAD-1189 independent acceptance checks (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! (a) Lock-free catalog reads never return a mix of old and new
//!     installation state while real upgrades publish: every answer is a
//!     state that was published, or a retryable `busy`.
//! (b) Dropping the PM lock from runtime admission must not reopen the gap
//!     legacy `app remove` documents: while an admission callback for an
//!     installation is running (it may create bindings, consent or runs),
//!     a remove of that installation does not proceed until it returns.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n---\n\n\
Why.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";

fn manifest(app: &str, version: u32) -> String {
    // Odd versions declare one connection slot, even versions none, so a
    // row whose version and slots disagree is a torn read.
    let connections = if version % 2 == 1 { "[cms]" } else { "[]" };
    format!(
        "---\napp: {app}\ntitle: Fixture\nversion: '{version}'\nneeds:\n  connections: {connections}\n---\n\nGuide v{version}.\n"
    )
}

fn bundle(dir: &Path, app: &str, version: u32) {
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::write(dir.join("app.md"), manifest(app, version)).unwrap();
    std::fs::write(dir.join("workflows").join("do.md"), WORKFLOW).unwrap();
}

fn consistent(row: &Value, digests: &BTreeMap<u32, String>) -> std::result::Result<(), String> {
    let version: u32 = row["version"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("no version: {row}"))?;
    let slots = row["connection_slots"].clone();
    let want = if version % 2 == 1 {
        json!(["cms"])
    } else {
        json!([])
    };
    if slots != want {
        return Err(format!("v{version} with slots {slots}: torn manifest"));
    }
    match digests.get(&version) {
        Some(d) if row["digest"].as_str() == Some(d.as_str()) => Ok(()),
        Some(d) => Err(format!(
            "v{version} digest {} != published {d}",
            row["digest"]
        )),
        None => Err(format!("v{version} was never published")),
    }
}

#[test]
fn cad1189_reads_never_mix_installation_states_during_upgrades() {
    let pm_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let pm = Pm::init(pm_dir.path()).unwrap();
    let v1 = sources.path().join("v1");
    bundle(&v1, "race-app", 1);
    let out = install(&pm, state.path(), v1.to_str().unwrap(), None).unwrap();
    let id = out["install_id"].as_str().unwrap().to_string();
    let first = show(&pm, &id).unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let digests = Arc::new(Mutex::new(BTreeMap::from([(1u32, first)])));

    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let (pm_dir, id, digests, done) =
            (pm.dir.clone(), id.clone(), digests.clone(), done.clone());
        std::thread::spawn(move || {
            let pm = Pm::at(&pm_dir).unwrap();
            let (mut ok, mut busy) = (0u32, 0u32);
            while !done.load(Ordering::SeqCst) {
                let answers = [list(&pm).map(|rows| rows[0].clone()), show(&pm, &id)];
                for answer in answers {
                    match answer {
                        Ok(row) => {
                            let known = digests.lock().unwrap().clone();
                            consistent(&row, &known).unwrap_or_else(|e| panic!("mixed state: {e}"));
                            ok += 1;
                        }
                        Err(e) if e.kind() == "busy" => busy += 1,
                        Err(e) => panic!("a racing read must retry or answer busy, got: {e}"),
                    }
                }
            }
            (ok, busy)
        })
    };

    for version in 2..=7u32 {
        let current = show(&pm, &id).unwrap();
        let digest = current["digest"].as_str().unwrap().to_string();
        let generation = current["catalog_generation"].as_str().unwrap().to_string();
        let source = sources.path().join(format!("v{version}"));
        bundle(&source, "race-app", version);
        let check =
            upgrade_check(&pm, &id, source.to_str().unwrap(), &digest, &generation).unwrap();
        let new_digest = check["digest"].as_str().unwrap().to_string();
        digests.lock().unwrap().insert(version, new_digest.clone());
        upgrade(
            &pm,
            &UpgradeRequest {
                id: &id,
                source: source.to_str().unwrap(),
                expected_digest: &digest,
                expected_generation: &generation,
                expected_new_digest: &new_digest,
                request_id: &format!("accept-up-{version}"),
            },
            |_, _| Ok(json!({})),
        )
        .unwrap();
    }
    done.store(true, Ordering::SeqCst);
    let (ok, busy) = reader.join().expect("reader saw a mixed state");
    assert!(
        ok > 0,
        "the reader must observe published states (busy={busy})"
    );
    let last = show(&pm, &id).unwrap();
    assert_eq!(last["version"], "7");
}

#[test]
fn cad1189_legacy_remove_waits_for_running_admission() {
    let pm_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let pm = Pm::init(pm_dir.path()).unwrap();
    crate::issue::write::project_add(
        &pm,
        "legacy",
        "LEG",
        &[repo.path().display().to_string()],
        &[],
        &[],
        None,
    )
    .unwrap();
    let src = sources.path().join("legacy-app");
    bundle(&src, "legacy-app", 2);
    app::install(&pm, "legacy", src.to_str().unwrap(), state.path(), "accept").unwrap();
    crate::issue::app_catalog::migrate_authorized(&pm).unwrap();
    let rows = list(&pm).unwrap();
    let id = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "legacy-app")
        .and_then(|r| r["install_id"].as_str())
        .expect("legacy installation is in the catalog")
        .to_string();

    let inside = Arc::new(Barrier::new(2));
    let admission_done = Arc::new(Mutex::new(None::<Instant>));
    let admission = {
        let (pm_dir, id, inside, admission_done) = (
            pm.dir.clone(),
            id.clone(),
            inside.clone(),
            admission_done.clone(),
        );
        std::thread::spawn(move || {
            let pm = Pm::at(&pm_dir).unwrap();
            with_runtime_snapshot(&pm, &id, |_, _| {
                inside.wait();
                // Stands in for the callback's store write (binding,
                // consent, run) against this installation.
                std::thread::sleep(Duration::from_millis(1500));
                *admission_done.lock().unwrap() = Some(Instant::now());
                Ok(())
            })
            .unwrap();
        })
    };
    inside.wait();
    let removed = app::remove(&pm, "legacy", "legacy-app", state.path(), "accept");
    let remove_done = Instant::now();
    admission.join().unwrap();
    let admission_done = admission_done.lock().unwrap().expect("admission finished");
    assert!(
        removed.is_err() || remove_done >= admission_done,
        "legacy remove completed while a runtime admission for the same \
         installation was still running: the PM lock no longer orders them \
         and nothing replaced it"
    );
}
