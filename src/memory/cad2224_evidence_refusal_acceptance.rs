//! CAD-2224 independent refusal acceptance. Authored from the ticket, not the
//! implementation: a wiki evidence read answers "definitely absent"
//! (`Ok(None)`) only after the pinned commit was actually read. A caller the
//! project's policy does not admit is refused with an error that is the same
//! for a private project and for one that does not exist, and the memory
//! resolver records such a citation as unreadable, never as missing.

use super::evidence::{analyse_as, resolve, Outcome, Resolution};
use super::policy::{EvidenceKind, EvidenceRef};
use crate::issue::Pm;
use crate::wiki::{read_revision_for_evidence, write, Caller};

const PAGE: &str = "projects/secret/notes.md";
const SECRET: &str = "private original text";

struct Fixture {
    _root: tempfile::TempDir,
    pm: Pm,
    revision: String,
}

fn fixture() -> Fixture {
    let root = tempfile::Builder::new().prefix("c2224").tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    let project = pm.dir.join("secret");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 3\n  visibility: private\n  members:\n  - users/mia\n",
    )
    .unwrap();
    write(&pm, &Caller::Operator, PAGE, SECRET, None).unwrap();
    let revision = crate::issue::git(&pm.dir, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(revision.len(), 40);
    Fixture {
        _root: root,
        pm,
        revision,
    }
}

fn mia() -> Caller {
    Caller::User("mia".into())
}

fn rob() -> Caller {
    Caller::User("rob".into())
}

fn wiki_ref(source: &str, revision: &str) -> EvidenceRef {
    EvidenceRef {
        kind: EvidenceKind::Wiki,
        source: source.into(),
        revision: revision.into(),
    }
}

#[test]
fn an_unadmitted_caller_is_refused_never_told_the_page_is_absent() {
    let f = fixture();
    let outcome = read_revision_for_evidence(&f.pm, &rob(), PAGE, &f.revision, 4096);
    assert!(
        outcome.is_err(),
        "an unadmitted read must be an error, not Ok(None) or content: {outcome:?}"
    );
    // The same holds for a page that is genuinely absent from the pinned
    // commit: absence is not a reply an outsider may receive.
    let absent = read_revision_for_evidence(
        &f.pm,
        &rob(),
        "projects/secret/absent.md",
        &f.revision,
        4096,
    );
    assert!(absent.is_err(), "{absent:?}");
    // A public (anonymous) caller fares no better.
    let public = read_revision_for_evidence(&f.pm, &Caller::Public, PAGE, &f.revision, 4096);
    assert!(public.is_err(), "{public:?}");
}

#[test]
fn a_private_project_and_a_nonexistent_one_refuse_identically() {
    let f = fixture();
    let private = read_revision_for_evidence(&f.pm, &rob(), PAGE, &f.revision, 4096)
        .unwrap_err()
        .to_string();
    let ghost_page = "projects/ghost/notes.md";
    let nonexistent = read_revision_for_evidence(&f.pm, &rob(), ghost_page, &f.revision, 4096)
        .unwrap_err()
        .to_string();
    // The refusals differ only by the path the caller itself supplied.
    assert_eq!(
        private.replace(PAGE, "<page>"),
        nonexistent.replace(ghost_page, "<page>"),
        "private and nonexistent projects must be indistinguishable"
    );
    assert!(!private.contains(SECRET), "{private}");
    assert!(!private.contains("private"), "{private}");
    assert!(!private.contains(&f.revision), "{private}");

    // A fresh tracker that has no such project at all answers the same way.
    let empty_root = tempfile::Builder::new().prefix("c2224e").tempdir().unwrap();
    let empty = Pm::init(&empty_root.path().join("pm")).unwrap();
    let from_empty = read_revision_for_evidence(&empty, &rob(), PAGE, &f.revision, 4096)
        .unwrap_err()
        .to_string();
    assert_eq!(private, from_empty);
}

#[test]
fn an_admitted_caller_gets_definite_absence_for_a_page_missing_at_the_commit() {
    let f = fixture();
    let found = read_revision_for_evidence(
        &f.pm,
        &mia(),
        "projects/secret/never-written.md",
        &f.revision,
        4096,
    )
    .expect("the pinned commit is readable, so absence is an answer, not an error");
    assert!(found.is_none(), "{found:?}");
    // An unknown commit is not absence: it stays an error.
    assert!(read_revision_for_evidence(&f.pm, &mia(), PAGE, &"0".repeat(40), 4096).is_err());
}

#[test]
fn an_admitted_caller_reads_the_content_at_the_pinned_revision() {
    let f = fixture();
    let value = read_revision_for_evidence(&f.pm, &mia(), PAGE, &f.revision, 4096)
        .unwrap()
        .expect("the page exists at the pinned commit");
    assert_eq!(value["kind"], "text");
    assert_eq!(value["text"], SECRET);
    assert_eq!(value["rev"], f.revision.as_str());
    // A later edit does not change what the pinned revision says.
    write(&f.pm, &Caller::Operator, PAGE, "edited later", None).unwrap();
    let again = read_revision_for_evidence(&f.pm, &mia(), PAGE, &f.revision, 4096)
        .unwrap()
        .unwrap();
    assert_eq!(again["text"], SECRET);
}

#[test]
fn the_resolver_records_an_unadmitted_citation_as_unreadable_not_missing() {
    let f = fixture();
    let (resolution, content) =
        resolve(&f.pm, "secret", &rob(), &wiki_ref(PAGE, &f.revision)).unwrap();
    assert!(content.is_none());
    match resolution {
        Resolution::Unsupported(why) => {
            assert!(!why.contains(SECRET), "{why}");
        }
        other => panic!("an unadmitted citation must be unreadable, got {other:?}"),
    }
    // Even a path that is absent at the commit is not "missing" to an outsider.
    let (absent, _) = resolve(
        &f.pm,
        "secret",
        &rob(),
        &wiki_ref("projects/secret/absent.md", &f.revision),
    )
    .unwrap();
    assert!(matches!(absent, Resolution::Unsupported(_)), "{absent:?}");

    // Positive controls: the admitted reader resolves it, and sees absence as
    // Missing only because the commit really was read.
    let (ok, bytes) = resolve(&f.pm, "secret", &mia(), &wiki_ref(PAGE, &f.revision)).unwrap();
    assert!(matches!(ok, Resolution::Resolved { .. }), "{ok:?}");
    assert_eq!(bytes.unwrap(), SECRET.as_bytes());
    let (missing, _) = resolve(
        &f.pm,
        "secret",
        &mia(),
        &wiki_ref("projects/secret/absent.md", &f.revision),
    )
    .unwrap();
    assert!(matches!(missing, Resolution::Missing(_)), "{missing:?}");
}

#[test]
fn the_analysis_skips_an_unadmitted_wiki_citation_and_never_passes() {
    let f = fixture();
    let mut mem = fixture_memory(vec![wiki_ref(PAGE, &f.revision)]);
    mem.project = "secret".into();
    let receipt = analyse_as(&f.pm, &mem, &rob()).unwrap();
    assert!(
        receipt.skipped.iter().any(|s| s.contains("wiki")),
        "{:?}",
        receipt.skipped
    );
    assert!(receipt.checked.is_empty(), "{:?}", receipt.checked);
    assert_ne!(receipt.outcome, Outcome::Pass);
    assert!(
        !receipt.findings.iter().any(|s| s.contains("absent")),
        "an unreadable citation must not be reported as absent: {:?}",
        receipt.findings
    );
    let joined = format!("{:?}{:?}", receipt.skipped, receipt.findings);
    assert!(!joined.contains(SECRET));
}

fn fixture_memory(refs: Vec<EvidenceRef>) -> super::Memory {
    super::Memory {
        project: "demo".into(),
        front: super::Front {
            id: "c2224".into(),
            kind: "rule".into(),
            status: "proposed".into(),
            scope: super::Scope::default(),
            source: None,
            confidence: "medium".into(),
            created: "2026-10-28T00:00:00Z".into(),
            verified_at: None,
            stale: None,
            supersedes: None,
            author: Some("fixture".into()),
            author_proof: None,
            contributors: Vec::new(),
            review_cycle: 0,
            active_operation: None,
            reviews: Vec::new(),
            finalizations: Vec::new(),
            policy: Some(super::policy::PolicyV2 {
                revision: 2,
                nature: super::policy::Nature::Judgement,
                provenance: super::policy::Provenance::Operator,
                audience: super::policy::Audience::Project,
                roles: Vec::new(),
                evidence_refs: refs,
                verification_condition: None,
                author: super::policy::Authorship {
                    actor: super::policy::ActorSnapshot::Operator { uid: 1000 },
                    model: None,
                },
                contributors: Vec::new(),
                attestations: Vec::new(),
                reviews: Vec::new(),
                finalizations: Vec::new(),
                feedback: Default::default(),
            }),
        },
        body: "Cites a private wiki page".into(),
        path: std::path::PathBuf::from("/nonexistent"),
    }
}
