//! CAD-1913 independent adversarial acceptance, authored from the ticket by
//! the acceptance author, NOT by the implementer. The implementer may not
//! edit or weaken this file.
//!
//! Ticket expectation, not implementation behavior. Eight rules are checked at
//! the *actual* delivery seam — `issue::dispatch::context_guard` for a plain
//! issue dispatch and `issue::dispatch::job_context` for an issue-linked job,
//! the two callers of the one native lesson selection — over a real tracker,
//! real registered projects, real Git repositories, real pinned project
//! context and real memory records accepted through the native policy path:
//!
//! 1. **Project isolation.** A lesson accepted in another project must never
//!    be selected for this issue, even when its scope names exactly the same
//!    planned repository path the issue declares. The negative is only
//!    meaningful next to its positive control: a lesson of the issue's *own*
//!    project, scoped to a path the issue declares and no commit has touched
//!    yet, is delivered. An in-project lesson whose scope reaches none of the
//!    issue's planned paths stays out, so the planned axis may not widen to
//!    "every lesson in the project". Plain issue and issue-linked job
//!    dispatch must select the same lesson ids.
//!
//! 2. **Confidential/secret non-disclosure.** Lesson reason text is
//!    file-controlled (a PM-directory writer can edit a `stale:` note without
//!    touching the semantic digest, and a memory file that fails to load puts
//!    its own name into the load-error fact), so it must never be echoed
//!    verbatim into the delivered context. A secret-shaped token planted in a
//!    withheld `stale:` reason or in a malformed memory file's name must not
//!    appear anywhere in the pinned manifest — documents, withheld gaps, their
//!    per-lesson notes — or in the planned text, while the withheld and
//!    unreadable facts stay identifiable and the delivery still succeeds.
//!
//! 3. **Caller-access boundary for citations.** The recipient of a delivery is
//!    an agent, and a stored citation reference is not proof that recipient can
//!    read the source. Only a decision the seam can actually make authorizes a
//!    pointer: a Wiki page the wiki ACL admits for an agent (`wiki::allowed`)
//!    is disclosed as a recorded pointer, and its bytes are never embedded. A
//!    `users/<handle>/` Wiki page — private to its member and the operator — is
//!    refused by that ACL, so its identifier is withheld. A Git citation is
//!    withheld as well: host-side facts (workspace visibility, the pinned
//!    reader) do not establish the recipient worker's own repository or
//!    filesystem access, so its identifier stays out until receiver authority
//!    exists. The lesson's own claim stays deliverable in every case.
//!
//! Also asserted: an accepted lesson with no finalized verify cycle is
//! labelled `unverified`, never `verified <date>`; acceptance alone is a
//! review, not an at-SHA re-check.
//!
//! 4. **Legacy dispatch outputs.** A project without the committed context
//!    opt-in takes the legacy lesson path, whose facts are written into two
//!    durable outputs: the committed issue comment and the dispatch report
//!    JSON. Both carry file-controlled reason text, so a real `dispatch::run`
//!    against a real daemon (registered fake seat, real tracker and repository)
//!    asserts on the actual `out["comment"]`, `out["lessons_withheld"]` and
//!    `out["lessons_error"]` values, with positive controls proving each path
//!    was exercised.
//!
//! 5. **Re-pinned manifests.** A pin written before redaction existed can carry
//!    a secret-bearing gap reason. Re-planning that manifest — on both the
//!    retained and the full path — must still report the gap while never
//!    copying the raw reason into the new supply receipt or the turn text.
//!
//! 6. **Wildcard planned declarations.** Planned paths are globs, so selection
//!    is the exact intersection of the lesson's scope glob with the declared
//!    glob, never a string prefix: a subtree declaration selects a concrete
//!    file inside it and a nested glob inside it, while a same-prefix but
//!    different subtree and a path below a single-segment declaration select
//!    nothing.
//!
//! 7. **Preserved axes and advisory conditions.** The planned-path axis is
//!    additive: a role-scoped lesson reaches only its role, a provider-only
//!    scope reaches only its provider, and a declared verification condition is
//!    shown as advisory, never as a verification or a result.
//!
//! 8. **Bounded overflow.** More matches than the injection cap are omitted
//!    from the source, the omitted lesson is named in a bounded gap, and the
//!    mandatory task body and required policy are delivered whole.
//!
//! Nothing here mocks the seam: `context_guard`/`job_context` are called
//! directly with the same captured issue snapshot the daemon dispatch paths
//! pass, and the planned context is re-planned over the same pinned blobs so
//! both the pin and the appended turn text are inspected. Isolated temp
//! tracker and repositories; production state is never touched.
//!
//! Run after the change:
//!   cargo test --features test-seam --test suite \
//!     cad1913_lesson_delivery_isolation_acceptance -- --nocapture --test-threads 1

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::issue::{write, Pm};
use crate::test_seam::{scoped, Asserted, Seam};

const PROJECT: &str = "alpha1913";
const PREFIX: &str = "C1913A";
const FOREIGN_PROJECT: &str = "beta1913";
const FOREIGN_PREFIX: &str = "C1913B";
const ISSUE: &str = "C1913A-1";

/// A file the issue declares it plans to touch. No fixture commit touches it,
/// so only the planned-path axis can select a lesson for it.
const PLANNED_PATH: &str = "src/cad1913/planned.rs";
/// An in-project lesson scope that reaches neither the planned path nor any
/// committed path.
const UNPLANNED_GLOB: &str = "docs/unplanned/**";

const MANIFEST_PATH: &str = "docs/cadence/project-context.yaml";
const POLICY_PATH: &str = "docs/policy.md";
const BODY: &str =
    "Execute the CAD-1913 acceptance fixture task using its required project policy.";

const OWN_SLUG: &str = "alpha-planned-lesson";
const OWN_CLAIM: &str = "OWN-PLANNED-CLAIM-1913";
const FOREIGN_SLUG: &str = "beta-planned-lesson";
const FOREIGN_CLAIM: &str = "FOREIGN-PLANNED-CLAIM-1913";
const UNPLANNED_SLUG: &str = "alpha-unplanned-lesson";
const UNPLANNED_CLAIM: &str = "UNPLANNED-CLAIM-1913";
const STALE_SLUG: &str = "alpha-stale-lesson";
const STALE_CLAIM: &str = "STALE-CLAIM-1913";
const CITED_SLUG: &str = "alpha-cited-lesson";
const CITED_CLAIM: &str = "CITED-CLAIM-1913";
/// A tracked documentation file of the issue's own repository, cited by a
/// lesson whose claim must still be delivered. Its identifier is withheld
/// because the seam cannot prove the recipient can read it, and its bytes are
/// never part of a delivery.
const CITED_PATH: &str = "docs/notes.md";
const CITED_CONTENT: &str = "CITED-BYTES-1913";
/// A Wiki citation to a `users/<handle>/` page: the wiki ACL refuses that
/// read to every non-member, so an agent recipient cannot read it.
const WIKI_SLUG: &str = "alpha-wiki-lesson";
const WIKI_CLAIM: &str = "WIKI-CLAIM-1913";
const WIKI_PRIVATE_SOURCE: &str = "users/alice/cad1913-private.md";
/// A Wiki citation the same ACL admits for an agent: `projects/<key>/`
/// reads are open.
const WIKI_AUTH_SLUG: &str = "alpha-wiki-authorized-lesson";
const WIKI_AUTH_CLAIM: &str = "WIKI-AUTH-CLAIM-1913";
/// A 40-hex immutable selector; the seam never resolves it.
const WIKI_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
/// A Git citation that escapes the issue's declared repository: the project's
/// own pinned reader refuses the path, so no worker lane can read it.
const ESCAPE_SLUG: &str = "alpha-escape-lesson";
const ESCAPE_CLAIM: &str = "ESCAPE-CLAIM-1913";
const ESCAPE_SOURCE: &str = "../beta-repo/private/plan.md";

// ── Fixture ──────────────────────────────────────────────────────

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = crate::reaper::output(Command::new("git").arg("-C").arg(cwd).args(args))
        .unwrap_or_else(|error| panic!("git {args:?} in {}: {error}", cwd.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.name=CAD-1913 acceptance",
            "-c",
            "user.email=cad1913-acceptance@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create the fixture repository");
    git(repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join(".gitignore"), ".cadence/\ntarget/\n").unwrap();
    fs::write(repo.join("tracked.txt"), "CAD-1913 lesson acceptance\n").unwrap();
    commit_all(repo, "fixture base");
}

/// [`init_repo`] plus a bare `origin` the lane cut can resolve.
fn init_repo_with_origin(repo: &Path) {
    init_repo(repo);
    let origin = PathBuf::from(format!("{}-origin.git", repo.display()));
    fs::create_dir_all(&origin).unwrap();
    git(
        &origin,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(
        repo,
        &["push", "--quiet", "--set-upstream", "origin", "main"],
    );
    git(repo, &["remote", "set-head", "origin", "main"]);
}

/// The committed project-context opt-in: one required policy document and
/// `agent_delivery: true`. Only real tracked files feed the pinned reader.
fn set_sources(repo: &Path, project: &str) {
    let policy = repo.join(POLICY_PATH);
    fs::create_dir_all(policy.parent().unwrap()).unwrap();
    fs::write(&policy, "The required policy body is present.\n").unwrap();
    let manifest = repo.join(MANIFEST_PATH);
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(
        &manifest,
        format!(
            "schema: 1\nproject: {project}\nagent_delivery: true\ndocuments:\n  \
             - id: policy\n    kind: scope\n    path: {POLICY_PATH}\n    \
             title: Required policy\n    required: true\n    roles: []\n"
        ),
    )
    .unwrap();
    commit_all(repo, "context opt-in");
}

fn issue_body() -> String {
    String::from(
        "Exercise the CAD-1913 planned-path lesson delivery.\n\n## Acceptance\n\n\
         - [ ] The selected lesson is delivered inside its own project only.\n",
    )
}

/// A valid memory body: one fact line, then the two required sections.
fn lesson_body(claim: &str) -> String {
    format!(
        "{claim}\n\n**Why:** recorded while proving the delivery seam\n\n\
         **How to apply:** apply it only inside the project that accepted it\n"
    )
}

/// A native agent author with a complete v2 identity proof. The daemon would
/// derive this from the socket peer; the fixture supplies the same shape, as
/// the memory module's own acceptance fixtures do.
fn author() -> crate::memory::policy::NativeActor {
    crate::memory::policy::NativeActor::Agent {
        identity: crate::memory::NativeIdentity {
            proof: crate::memory::IdentityProof {
                alias: "cad1913-author".to_string(),
                registration: 11,
                generation: "gen-11".to_string(),
                process_start: 111,
                role: "worker".to_string(),
                uid: Some("uid-cad1913-author".to_string()),
            },
        },
        model: None,
    }
}

fn operator() -> crate::memory::policy::NativeActor {
    crate::memory::policy::NativeActor::Operator { uid: 1000 }
}

fn planned_scope() -> crate::memory::Scope {
    crate::memory::Scope {
        paths: vec![PLANNED_PATH.to_string()],
        ..Default::default()
    }
}

/// Propose through the native policy path and finalize acceptance as the
/// operator — exactly the supported route a real lesson takes. The result is
/// asserted retrieval-eligible, so every negative assertion below is a real
/// refusal of a deliverable lesson rather than a record that was invalid all
/// along.
fn accept_lesson(pm: &Pm, project: &str, slug: &str, claim: &str, scope: crate::memory::Scope) {
    accept_lesson_with(
        pm,
        project,
        slug,
        claim,
        scope,
        crate::memory::policy::ProposalOptions::default(),
    );
}

fn accept_lesson_with(
    pm: &Pm,
    project: &str,
    slug: &str,
    claim: &str,
    scope: crate::memory::Scope,
    options: crate::memory::policy::ProposalOptions,
) {
    let proposed = crate::memory::propose_native(
        pm,
        project,
        "rule",
        scope,
        None,
        Some("high"),
        None,
        Some(&lesson_body(claim)),
        Some(slug),
        options,
        &author(),
    )
    .expect("propose the fixture lesson");
    let digest = proposed["digest"]
        .as_str()
        .expect("the proposal reports its semantic digest")
        .to_string();
    crate::memory::finalize_native(
        pm,
        Some(project),
        slug,
        "accept",
        &digest,
        None,
        &operator(),
    )
    .expect("operator acceptance of the fixture lesson");
    let (_, loaded) =
        crate::memory::find(pm, Some(project), slug).expect("load the accepted fixture lesson");
    let (eligible, reason) = crate::memory::retrieval_status(&loaded);
    assert!(
        eligible,
        "fixture lesson `{slug}` must be retrieval-eligible, not withheld: {reason}"
    );
}

struct Fixture {
    _dir: tempfile::TempDir,
    pm: Pm,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1913-lessons-")
            .tempdir()
            .expect("isolated CAD-1913 root");
        let repo = dir.path().join("alpha-repo");
        let foreign_repo = dir.path().join("beta-repo");
        init_repo(&repo);
        init_repo(&foreign_repo);
        let pm = Pm::init(&dir.path().join("pm")).expect("initialize the isolated tracker");
        write::project_add(
            &pm,
            PROJECT,
            PREFIX,
            &[repo.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("register the issue's own project");
        write::project_add(
            &pm,
            FOREIGN_PROJECT,
            FOREIGN_PREFIX,
            &[foreign_repo.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("register the foreign project");
        // Both projects opt in, so the foreign project's memory pool is real
        // and readable: its lesson must be excluded by project scope, never by
        // an unconfigured or unreadable project.
        set_sources(&repo, PROJECT);
        set_sources(&foreign_repo, FOREIGN_PROJECT);
        write::new_issue(
            &pm,
            &repo,
            Some(PROJECT),
            "CAD-1913 planned-path delivery fixture",
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(ISSUE),
            Some(issue_body().as_str()),
            "cad1913-acceptance",
        )
        .expect("create the fixture issue");
        write::set_fields(
            &pm,
            &[ISSUE.to_string()],
            &[format!("paths={PLANNED_PATH}")],
            "cad1913-acceptance",
            None,
        )
        .expect("declare the issue's planned path");
        Fixture {
            _dir: dir,
            pm,
            repo,
        }
    }
}

// ── The delivery seam ────────────────────────────────────────────

/// One seam delivery: the pinned manifest, the exact body the turn sends, and
/// the appended context the plan produced for it.
struct SeamDelivery {
    manifest: crate::context_pack::Manifest,
    body: String,
    text: String,
}

/// The plain issue dispatch seam: the same `context_guard` call the daemon
/// dispatch RPC and `dispatch::run` make, over the captured issue snapshot.
fn issue_seam(fixture: &Fixture) -> SeamDelivery {
    issue_seam_for(fixture, Some("dev"), "fake")
}

/// [`issue_seam`] with the assignee's runtime facts made explicit.
fn issue_seam_for(fixture: &Fixture, team_role: Option<&str>, provider: &str) -> SeamDelivery {
    let gated = crate::issue::dispatch::gate_issue(&fixture.pm, ISSUE, false)
        .expect("the fixture issue gates as a plain dispatch");
    let prepared = crate::issue::dispatch::context_guard(
        &fixture.pm,
        &gated.project,
        &gated.front,
        crate::issue::dispatch::ContextTarget {
            team_role,
            provider,
            endpoint_kind: "fake",
        },
        false,
        || {
            Ok((
                crate::context_pack::TaskContract::new(
                    &gated.front.id,
                    &gated.front.title,
                    gated.source.as_bytes(),
                    &gated.items,
                    gated.request.clone(),
                ),
                std::borrow::Cow::Borrowed(BODY),
            ))
        },
    )
    .expect("an opted-in project with a readable required policy must not refuse")
    .expect("the fixture project is explicitly opted in to context delivery");
    let root = crate::issue::context::repository_root(&gated.project)
        .expect("the fixture project has one resolvable repository");
    let mut reader = |source: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &root,
            &source.revision,
            &source.path,
            &source.digest,
        )
    };
    let plan = crate::context_pack::plan(
        &prepared.manifest,
        None,
        false,
        prepared.body.as_ref(),
        0,
        &mut reader,
    )
    .expect("the delivered context plans over its pinned blobs");
    SeamDelivery {
        manifest: prepared.manifest,
        body: prepared.body.into_owned(),
        text: plan.text,
    }
}

fn lesson_source(manifest: &crate::context_pack::Manifest) -> Option<&crate::context_pack::Source> {
    manifest
        .documents
        .iter()
        .find(|document| document.kind == "lessons")
}

fn lesson_ids(manifest: &crate::context_pack::Manifest) -> Vec<String> {
    lesson_source(manifest)
        .map(|source| source.lesson_ids.clone())
        .unwrap_or_default()
}

/// Everything the turn delivers: the pinned manifest (documents, inline
/// lesson render, withheld gaps and their per-lesson notes) plus the planned
/// text.
fn delivered(manifest: &crate::context_pack::Manifest, text: &str) -> String {
    format!(
        "{}\n{text}",
        serde_json::to_string(manifest).expect("the manifest serializes")
    )
}

fn assert_absent(delivered: &str, needle: &str, what: &str) {
    assert!(
        !delivered.contains(needle),
        "{what} (`{needle}`) reached the delivered context:\n{delivered}"
    );
}

/// Append a `stale:` mark to an already accepted record. The field is outside
/// the semantic digest, so the record keeps its eligibility and the delivery
/// seam sees a withheld lesson carrying this exact reason — the disclosure
/// path this check exists for.
fn plant_stale_reason(pm: &Pm, slug: &str, token: &str) {
    let path = crate::memory::memory_dir(pm, PROJECT).join(format!("{slug}.md"));
    let text = fs::read_to_string(&path).expect("read the accepted lesson file");
    let (yaml, body) = crate::issue::parse::split_front(&text).expect("split the lesson front");
    let mut yaml = yaml.to_string();
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    yaml.push_str(&format!(
        "stale: \"evidence marked stale: rotated {token}\"\n"
    ));
    fs::write(&path, format!("---\n{yaml}---\n{body}")).expect("plant the stale mark");
}

// ── Checks ───────────────────────────────────────────────────────

/// The core isolation rule: a lesson from another project can never be
/// selected by a planned path shared with this issue, and the positive
/// control proves the planned axis is actually live before any commit exists.
#[test]
fn cad1913_planned_path_selection_is_isolated_to_the_issues_own_project() {
    let fixture = Fixture::new();
    accept_lesson(&fixture.pm, PROJECT, OWN_SLUG, OWN_CLAIM, planned_scope());
    accept_lesson(
        &fixture.pm,
        FOREIGN_PROJECT,
        FOREIGN_SLUG,
        FOREIGN_CLAIM,
        planned_scope(),
    );
    accept_lesson(
        &fixture.pm,
        PROJECT,
        UNPLANNED_SLUG,
        UNPLANNED_CLAIM,
        crate::memory::Scope {
            paths: vec![UNPLANNED_GLOB.to_string()],
            ..Default::default()
        },
    );

    let seam = issue_seam(&fixture);
    let (manifest, text) = (seam.manifest, seam.text);
    let delivery = delivered(&manifest, &text);

    // Positive control: the issue's own planned path selects before any commit.
    let source = lesson_source(&manifest)
        .expect("the planned-path lesson must be delivered as the optional lesson source");
    assert_eq!(
        source.lesson_ids,
        vec![OWN_SLUG.to_string()],
        "only the issue's own planned-path lesson may be selected"
    );
    assert!(
        delivery.contains(OWN_CLAIM),
        "the delivered lesson context must carry the selected lesson's claim\n{delivery}"
    );
    // Acceptance is a review, not an at-SHA re-check.
    assert!(
        delivery.contains("unverified"),
        "an accepted lesson with no finalized verify cycle must be labelled unverified\n{delivery}"
    );
    assert!(
        !delivery.contains("verified 20"),
        "no at-SHA verified claim may be invented for an unverified lesson\n{delivery}"
    );

    // Adversarial: the foreign project's accepted lesson names the very same
    // planned path, and must still be absent everywhere.
    assert_absent(&delivery, FOREIGN_CLAIM, "a foreign project's lesson claim");
    assert_absent(&delivery, FOREIGN_SLUG, "a foreign project's lesson id");
    // And the planned axis must not widen to every lesson of the project.
    assert_absent(
        &delivery,
        UNPLANNED_CLAIM,
        "an in-project lesson whose scope reaches no planned path",
    );
    assert_absent(
        &delivery,
        UNPLANNED_SLUG,
        "an in-project out-of-scope lesson id",
    );

    // Deterministic: the same issue read twice selects the same lessons and
    // renders the same bytes — no clock, hash order or ambient state.
    let again = issue_seam(&fixture);
    assert_eq!(
        serde_json::to_string(&manifest).expect("the manifest serializes"),
        serde_json::to_string(&again.manifest).expect("the manifest serializes"),
        "the delivered lesson selection must be deterministic"
    );
    assert_eq!(
        text, again.text,
        "the planned turn text must be deterministic"
    );
}

/// File-controlled reason text must never be echoed raw: a secret-shaped token
/// planted in a withheld lesson's `stale:` note stays out of the delivery,
/// while the withholding itself remains visible and bounded.
#[test]
fn cad1913_withheld_lesson_reason_never_discloses_a_secret() {
    let fixture = Fixture::new();
    accept_lesson(&fixture.pm, PROJECT, OWN_SLUG, OWN_CLAIM, planned_scope());
    accept_lesson(
        &fixture.pm,
        PROJECT,
        STALE_SLUG,
        STALE_CLAIM,
        planned_scope(),
    );
    // Assembled at runtime so this source tree carries no credential shape.
    let token = format!("figd_{}", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5h");
    plant_stale_reason(&fixture.pm, STALE_SLUG, &token);

    // The plant is realistic: the mark is outside the semantic digest, so the
    // record stays eligible and the seam withholds it *with its reason*.
    let (_, loaded) = crate::memory::find(&fixture.pm, Some(PROJECT), STALE_SLUG)
        .expect("load the planted lesson");
    assert!(
        crate::memory::retrieval_status(&loaded).0,
        "the planted stale mark must not void eligibility, or this check would not \
         exercise the reason-disclosure path"
    );
    assert!(
        loaded
            .front
            .stale
            .as_deref()
            .unwrap_or_default()
            .contains(&token),
        "the planted reason must actually carry the token"
    );

    // Second file-controlled path: a memory file that fails to load. The load
    // error names the file, so a PM-directory writer puts text of its choosing
    // into the delivery's load-error fact — the same text the dispatch report
    // carries as `lessons_error`.
    let malformed =
        crate::memory::memory_dir(&fixture.pm, PROJECT).join(format!("{token}-notes.md"));
    fs::write(&malformed, "not a memory file\n").expect("plant the malformed lesson file");

    let seam = issue_seam(&fixture);
    let (manifest, text) = (seam.manifest, seam.text);
    let delivery = delivered(&manifest, &text);

    assert!(
        delivery.contains(OWN_CLAIM),
        "the control lesson must still be delivered"
    );
    assert!(
        manifest.withheld.iter().any(|gap| gap.state == "withheld"),
        "the withheld lesson must stay observable as a bounded receipt fact"
    );
    assert!(
        manifest
            .withheld
            .iter()
            .any(|gap| gap.state == "unreadable"),
        "the malformed lesson file must surface as a bounded load-error fact"
    );
    assert!(
        delivery.contains(STALE_SLUG),
        "a withheld lesson must stay identifiable in its bounded receipt"
    );
    // Non-disclosure: never echo file-controlled reason text verbatim, from a
    // withheld reason or from a load error that names the offending file.
    assert!(
        !delivery.contains(&token),
        "the delivered context echoed a secret-shaped token from file-controlled \
         lesson text (withheld reason or load error)"
    );
}

/// Issue/job parity: the issue-linked job seam selects exactly the same
/// lessons as the plain issue seam, and inherits the same isolation.
#[test]
fn cad1913_issue_linked_job_seam_matches_the_issue_seam() {
    let fixture = Fixture::new();
    accept_lesson(&fixture.pm, PROJECT, OWN_SLUG, OWN_CLAIM, planned_scope());
    accept_lesson(
        &fixture.pm,
        FOREIGN_PROJECT,
        FOREIGN_SLUG,
        FOREIGN_CLAIM,
        planned_scope(),
    );

    let issue_seam_delivery = issue_seam(&fixture);
    let (issue_manifest, issue_text) = (issue_seam_delivery.manifest, issue_seam_delivery.text);
    assert_eq!(
        lesson_ids(&issue_manifest),
        vec![OWN_SLUG.to_string()],
        "the plain issue seam must select only the issue's own planned-path lesson"
    );

    let gated = crate::issue::dispatch::capture_issue(&fixture.pm, ISSUE)
        .expect("capture the fixture issue")
        .expect("the fixture issue is held by its project");
    let spec = fixture.pm.dir.join(PROJECT).join(ISSUE).join("issue.md");
    let job = crate::store::Job {
        id: "cad1913-job".to_string(),
        title: None,
        spec_path: spec.display().to_string(),
        spec_sha256: None,
        pm_alias: "cad1913-pm".to_string(),
        issue_id: Some(ISSUE.to_string()),
        repo: Some(fixture.repo.display().to_string()),
        base_ref: None,
        state: "open".to_string(),
        max_revisions: 1,
        stall_secs: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    let task = crate::store::Task {
        id: "cad1913-task".to_string(),
        job_id: job.id.clone(),
        title: None,
        role: "dev".to_string(),
        assignee: Some("cad1913-worker".to_string()),
        spec_path: None,
        acceptance: None,
        worktree: None,
        branch: None,
        base_sha: None,
        head_sha: None,
        state: "dispatched".to_string(),
        revision: 1,
        dispatch_message: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    let prepared = crate::issue::dispatch::job_context(
        &fixture.pm,
        gated,
        crate::issue::dispatch::JobContext {
            job: &job,
            task: &task,
            revision: 1,
            message: "cad1913-fixture-message",
            target: crate::issue::dispatch::ContextTarget {
                team_role: Some("dev"),
                provider: "fake",
                endpoint_kind: "fake",
            },
            // A preview: the lane proof belongs to the daemon's own admission
            // and is not what this check is about.
            admission_scope: None,
        },
        false,
    )
    .expect("the issue-linked job context must not refuse")
    .expect("the fixture project is opted in to context delivery");

    assert_eq!(
        lesson_ids(&prepared.manifest),
        lesson_ids(&issue_manifest),
        "plain issue dispatch and issue-linked job dispatch must select the same lessons"
    );
    let job_delivered = format!(
        "{}\n{}",
        serde_json::to_string(&prepared.manifest).expect("the job manifest serializes"),
        prepared.body
    );
    assert!(
        job_delivered.contains(OWN_CLAIM),
        "the job kickoff must carry the selected lesson's claim\n{job_delivered}"
    );
    assert_absent(
        &job_delivered,
        FOREIGN_CLAIM,
        "a foreign project's lesson claim in the job kickoff",
    );
    assert_absent(
        &job_delivered,
        FOREIGN_SLUG,
        "a foreign project's lesson id in the job kickoff",
    );
    // The issue seam's own delivery is unchanged by the job run.
    assert_absent(
        &delivered(&issue_manifest, &issue_text),
        FOREIGN_CLAIM,
        "a foreign project's lesson claim in the issue kickoff",
    );
}

/// Caller-access boundary for citations. The recipient of a delivery is an
/// agent, and a stored reference is not proof the recipient can read the
/// source. Only a decision the seam can actually make counts:
///
/// * a Wiki citation the wiki ACL admits for an agent (`projects/<key>/…`) is
///   disclosed as a recorded pointer — `wiki::allowed` is a pure decision over
///   the caller spelling and the path;
/// * a Wiki citation to `users/<handle>/…` is refused by that same ACL, so its
///   identifier is withheld while the lesson's claim stays deliverable;
/// * a Git citation is withheld even when the daemon host can read it. This
///   fixture proves host-side facts only — `Project::allows` workspace
///   visibility and `issue::context::read_pinned_blob` — and neither
///   establishes the recipient worker's own filesystem/repository confinement.
///   Until receiver authority is established, the identifier stays out; an
///   escaping `../` path is withheld for the same reason plus containment.
///
/// No cited bytes are ever embedded, and no recorded pointer is presented as
/// verified evidence.
#[test]
fn cad1913_citation_identifier_is_withheld_without_recipient_access() {
    let fixture = Fixture::new();
    // A tracked documentation file of the issue's own repository, committed at
    // the revision the Git citation names.
    let cited = fixture.repo.join(CITED_PATH);
    fs::create_dir_all(cited.parent().unwrap()).unwrap();
    let cited_text = format!("{CITED_CONTENT}\n");
    fs::write(&cited, &cited_text).unwrap();
    commit_all(&fixture.repo, "cited file");
    let revision = git(&fixture.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    // The only recipient-access decision this seam can make is the wiki ACL.
    // It admits a projects/ page for an agent and refuses a users/ page; the
    // Git facts below are host-side readability only and are deliberately not
    // treated as authorization.
    let wiki_agent = crate::wiki::Caller::Agent {
        alias: "cad1913-worker".to_string(),
        project: Some(PROJECT.to_string()),
    };
    let wiki_auth_source = format!("projects/{PROJECT}/plan.md");
    assert!(
        crate::wiki::allowed(
            &wiki_agent,
            crate::wiki::Op::Read,
            &["projects", PROJECT, "plan.md"]
        )
        .is_ok(),
        "a projects/ wiki page is readable by an agent"
    );
    assert!(
        crate::wiki::allowed(
            &wiki_agent,
            crate::wiki::Op::Read,
            &["users", "alice", "cad1913-private.md"]
        )
        .is_err(),
        "a users/ wiki page is private to its member and the operator"
    );

    let git_ref = |source: &str| crate::memory::policy::ProposalOptions {
        evidence_refs: vec![crate::memory::policy::EvidenceRef {
            kind: crate::memory::policy::EvidenceKind::Git,
            source: source.to_string(),
            revision: revision.clone(),
        }],
        ..Default::default()
    };
    let wiki_ref = |source: &str| crate::memory::policy::ProposalOptions {
        evidence_refs: vec![crate::memory::policy::EvidenceRef {
            kind: crate::memory::policy::EvidenceKind::Wiki,
            source: source.to_string(),
            revision: WIKI_REVISION.to_string(),
        }],
        ..Default::default()
    };
    accept_lesson_with(
        &fixture.pm,
        PROJECT,
        CITED_SLUG,
        CITED_CLAIM,
        planned_scope(),
        git_ref(CITED_PATH),
    );
    accept_lesson_with(
        &fixture.pm,
        PROJECT,
        WIKI_AUTH_SLUG,
        WIKI_AUTH_CLAIM,
        planned_scope(),
        wiki_ref(&wiki_auth_source),
    );
    accept_lesson_with(
        &fixture.pm,
        PROJECT,
        WIKI_SLUG,
        WIKI_CLAIM,
        planned_scope(),
        wiki_ref(WIKI_PRIVATE_SOURCE),
    );
    accept_lesson_with(
        &fixture.pm,
        PROJECT,
        ESCAPE_SLUG,
        ESCAPE_CLAIM,
        planned_scope(),
        git_ref(ESCAPE_SOURCE),
    );

    let seam = issue_seam(&fixture);
    let (manifest, text) = (seam.manifest, seam.text);
    let delivery = delivered(&manifest, &text);

    // Established access is disclosed as a recorded pointer, never as resolved
    // bytes and never as a verification.
    assert!(
        delivery.contains(&wiki_auth_source),
        "an authorized Wiki citation must stay visible as a recorded pointer\n{delivery}"
    );
    assert!(
        !delivery.contains(CITED_CONTENT),
        "the delivered context embedded cited file bytes instead of a recorded pointer"
    );
    assert!(
        !delivery.contains("verified 20"),
        "a recorded citation is never an at-SHA verification\n{delivery}"
    );

    // A Git citation is withheld: the seam proves host-side readability, not
    // the recipient worker's own repository/filesystem access. The lesson's
    // own claim stays deliverable.
    assert!(
        delivery.contains(CITED_CLAIM),
        "the Git-citing lesson's own claim stays deliverable\n{delivery}"
    );
    assert!(
        !delivery.contains(CITED_PATH),
        "a Git citation was disclosed without establishing the recipient's access"
    );

    // Access is not established for a private users/ page: identifier and any
    // content stay out, while the lesson's own claim remains deliverable.
    assert!(
        delivery.contains(WIKI_CLAIM),
        "the wiki lesson's own claim stays deliverable\n{delivery}"
    );
    assert!(
        !delivery.contains(WIKI_PRIVATE_SOURCE),
        "a citation the recipient cannot read disclosed its identifier"
    );

    // Nor is it established for a path that escapes the declared repository.
    assert!(
        delivery.contains(ESCAPE_CLAIM),
        "the escaping lesson's own claim stays deliverable\n{delivery}"
    );
    assert!(
        !delivery.contains(ESCAPE_SOURCE),
        "a citation outside the project's declared repository disclosed its identifier"
    );
}

// ── Real dispatch run: the legacy comment and report ────────────

const LEGACY_WORKER: &str = "cad1913-legacy-worker";
const LEGACY_PM: &str = "cad1913-legacy-pm";

/// A real daemon with a registered fake seat over a real tracker and
/// repository, and **no** committed context manifest — so the dispatch takes
/// the legacy lesson path whose facts feed the committed comment and the
/// dispatch report JSON.
struct DispatchFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    pm: Pm,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for DispatchFixture {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("fixture daemon panicked");
            }
        }
    }
}

impl DispatchFixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1913-legacy-")
            .tempdir()
            .expect("isolated CAD-1913 dispatch root");
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        init_repo_with_origin(&repo);
        let pm_dir = root.join("pm");
        let pm = Pm::init(&pm_dir).expect("initialize the isolated tracker");
        write::project_add(
            &pm,
            PROJECT,
            PREFIX,
            &[repo.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("register the issue's own project");
        write::new_issue(
            &pm,
            &repo,
            Some(PROJECT),
            "CAD-1913 legacy lesson disclosure fixture",
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(ISSUE),
            Some(issue_body().as_str()),
            "cad1913-acceptance",
        )
        .expect("create the fixture issue");

        let stop = Arc::new(AtomicBool::new(false));
        let env = crate::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let opts = crate::daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(crate::daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let state = root.join("s");
        let mut threads = Vec::new();
        let serve_state = state.clone();
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&serve_state, opts).expect("fixture daemon");
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while crate::client::rpc_timeout(
            &state,
            "health",
            serde_json::json!({}),
            Duration::from_secs(2),
        )
        .is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(Instant::now() < deadline, "fixture daemon startup timeout");
            std::thread::sleep(Duration::from_millis(50));
        }

        let fixture = DispatchFixture {
            _dir: dir,
            root,
            pm,
            stop,
            threads,
        };
        fixture.op(
            "agent_register",
            serde_json::json!({
                "alias": LEGACY_PM, "provider": "inbox",
                "endpoint_kind": "inbox", "role": "pm",
            }),
        );
        fixture.op(
            "agent_register",
            serde_json::json!({
                "alias": LEGACY_WORKER, "provider": "fake",
                "endpoint_kind": "fake", "role": "worker", "cwd": fixture.repo(),
            }),
        );
        fixture.wait_state(LEGACY_WORKER, "idle");
        fixture
    }

    fn state(&self) -> PathBuf {
        self.root.join("s")
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn op(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        scoped(Asserted::Operator, || {
            crate::client::rpc(&self.state(), method, params)
        })
        .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    fn wait_state(&self, alias: &str, state: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let show = self.op("agent_show", serde_json::json!({"alias": alias}));
            if show["agent"]["state"] == state {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{alias} never became {state}: {show}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The legacy dispatch outputs are durable and readable after the turn: the
/// committed issue comment and the dispatch report JSON. Both carry
/// file-controlled lesson reason text, so both are disclosure surfaces. This
/// drives the real `dispatch::run` against a real daemon and asserts on the
/// actual `out["comment"]`, `out["lessons_withheld"]` and `out["lessons_error"]`
/// values, with positive controls proving each path was exercised.
#[test]
fn cad1913_legacy_dispatch_comment_and_report_never_disclose_lesson_secrets() {
    let fixture = DispatchFixture::new();
    // Project-wide scope, so selection does not depend on commit or planned
    // paths: this check is about the outputs, not the matching axis.
    accept_lesson(
        &fixture.pm,
        PROJECT,
        STALE_SLUG,
        STALE_CLAIM,
        crate::memory::Scope {
            project: true,
            ..Default::default()
        },
    );
    let token = format!("figd_{}", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5h");
    plant_stale_reason(&fixture.pm, STALE_SLUG, &token);
    let malformed =
        crate::memory::memory_dir(&fixture.pm, PROJECT).join(format!("{token}-notes.md"));
    fs::write(&malformed, "not a memory file\n").expect("plant the malformed lesson file");

    let args = crate::issue::dispatch::DispatchArgs {
        to: LEGACY_WORKER.to_string(),
        note: None,
        name: None,
        base: None,
        repo: Some(fixture.repo()),
        reply_to: Some(LEGACY_PM.to_string()),
        summary: None,
        job_spec: None,
        no_lessons: false,
        force: false,
        take_over: None,
    };
    let out = scoped(Asserted::Operator, || {
        crate::issue::dispatch::run(
            &fixture.pm,
            ISSUE,
            &args,
            "operator",
            &fixture.state(),
            None,
            crate::issue::dispatch::DispatchOrigin::Explicit,
        )
    })
    .expect("the legacy dispatch must run against the fixture daemon");
    assert!(
        out["message"].is_string(),
        "the fixture dispatch must actually reach the seat: {out}"
    );

    let withheld = serde_json::to_string(&out["lessons_withheld"]).expect("withheld serializes");
    let error = serde_json::to_string(&out["lessons_error"]).expect("error serializes");
    // `out["comment"]` is the committed comment's file name; the durable bytes
    // are what a later reader (or the board) sees, so read them.
    let comment_file = out["comment"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| panic!("the dispatch must name its committed comment: {out}"));
    let comment_path = fixture
        .pm
        .dir
        .join(PROJECT)
        .join(ISSUE)
        .join("comments")
        .join(comment_file);
    let comment = fs::read_to_string(&comment_path).unwrap_or_else(|error| {
        panic!(
            "read the committed dispatch comment {}: {error}",
            comment_path.display()
        )
    });

    // Positive controls: both file-controlled paths reached the real outputs.
    assert!(
        withheld.contains(STALE_SLUG),
        "the withheld lesson must be reported in the dispatch report: {out}"
    );
    assert!(
        comment.contains(STALE_SLUG),
        "the committed comment must name the withheld lesson: {comment}"
    );
    assert!(
        out["lessons_error"].is_string(),
        "the malformed lesson file must be reported as a load error: {out}"
    );

    // Non-disclosure on every durable output of this dispatch.
    assert!(
        !comment.contains(&token),
        "the committed dispatch comment echoed a secret-shaped token from \
         file-controlled lesson text"
    );
    assert!(
        !withheld.contains(&token),
        "the dispatch report's lessons_withheld echoed a secret-shaped token"
    );
    assert!(
        !error.contains(&token),
        "the dispatch report's lessons_error echoed a secret-shaped token from \
         file-controlled lesson text"
    );
}

/// An existing pin can carry a gap reason written before redaction existed.
/// Re-planning that manifest must not copy the raw text into the new supply
/// receipt or the turn text — the receipt is a fresh output even when the pin
/// it describes is old.
#[test]
fn cad1913_repinned_manifest_gap_never_rediscloses_a_secret() {
    let fixture = Fixture::new();
    accept_lesson(&fixture.pm, PROJECT, OWN_SLUG, OWN_CLAIM, planned_scope());
    let mut manifest = issue_seam(&fixture).manifest;
    let token = format!("figd_{}", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5h");
    // The shape an older pin carries: one aggregate gap with free-text reason
    // and no per-lesson detail.
    manifest.withheld.push(crate::context_pack::Gap {
        id: "native:lessons-withheld".to_string(),
        path: "memory://accepted-lessons".to_string(),
        state: "withheld".to_string(),
        reason: format!("1 in-scope lesson(s) withheld; recorded note: {token}"),
        required: false,
        lessons: Vec::new(),
    });

    let gated = crate::issue::dispatch::gate_issue(&fixture.pm, ISSUE, false)
        .expect("the fixture issue gates as a plain dispatch");
    let root = crate::issue::context::repository_root(&gated.project)
        .expect("the fixture project has one resolvable repository");
    let mut reader = |source: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &root,
            &source.revision,
            &source.path,
            &source.digest,
        )
    };

    // A previously pinned receipt that confirms the required document, so the
    // re-plan takes the retained branch and carries the old gap metadata.
    let required = manifest
        .documents
        .iter()
        .find(|document| document.required)
        .expect("the fixture manifest has a required policy document");
    let resident = serde_json::json!({
        "id": required.id, "path": required.path, "revision": required.revision,
        "digest": required.digest, "bytes": required.bytes,
        "required": required.required, "disposition": "appended",
    });
    let previous = {
        let mut previous = crate::context_pack::Receipt {
            task: manifest.task.task.clone(),
            task_revision: manifest.task.revision.clone(),
            manifest: manifest.digest.clone(),
            sources: vec![resident],
            ..Default::default()
        };
        // An older receipt also carries its own runtime gap for the optional
        // lesson source, written before redaction existed. The retained branch
        // re-emits that entry verbatim.
        if let Some(lesson) = lesson_source(&manifest) {
            previous.withheld.push(serde_json::json!({
                "id": lesson.id, "path": lesson.path, "state": "overflow",
                "reason": format!("optional source omitted; recorded note: {token}"),
                "required": false,
            }));
        }
        previous
    };
    let carried = lesson_source(&manifest).map(|lesson| lesson.id.clone());

    for (label, receipt, resident_turn) in
        [("retained", Some(&previous), true), ("full", None, false)]
    {
        let plan =
            crate::context_pack::plan(&manifest, receipt, resident_turn, BODY, 0, &mut reader)
                .unwrap_or_else(|refusal| panic!("{label} re-plan refused: {refusal:?}"));
        let withheld = serde_json::to_string(&plan.withheld).expect("withheld serializes");
        // Positive control: the gap itself is still reported, so the absence
        // below is a redaction and not a dropped fact.
        assert!(
            withheld.contains("native:lessons-withheld"),
            "the {label} supply receipt must still report the carried gap: {withheld}"
        );
        if label == "retained" {
            let carried = carried.as_deref().expect("the fixture has a lesson source");
            assert!(
                withheld.contains(carried),
                "the retained receipt must still report the carried prior gap: {withheld}"
            );
        }
        assert!(
            !withheld.contains(&token),
            "the {label} supply receipt re-disclosed a secret-bearing gap reason from an \
             existing pin"
        );
        assert!(
            !plan.text.contains(&token),
            "the {label} turn text re-disclosed a secret-bearing gap reason from an existing pin"
        );
    }
}

// ── Planned wildcard declarations ───────────────────────────────

/// A subtree declaration: every path below `src/cad1913/`.
const WILDCARD_SUBTREE: &str = "src/cad1913/**";
/// A single-segment declaration: only files directly under `src/other1913/`.
const WILDCARD_SIBLING: &str = "src/other1913/*.rs";

const OVERLAP_FILE_SLUG: &str = "alpha-overlap-file-lesson";
const OVERLAP_FILE_CLAIM: &str = "OVERLAP-FILE-CLAIM-1913";
const OVERLAP_GLOB_SLUG: &str = "alpha-overlap-glob-lesson";
const OVERLAP_GLOB_CLAIM: &str = "OVERLAP-GLOB-CLAIM-1913";
const PREFIX_SLUG: &str = "alpha-prefix-lesson";
const PREFIX_CLAIM: &str = "PREFIX-DISJOINT-CLAIM-1913";
const SIBLING_SLUG: &str = "alpha-sibling-lesson";
const SIBLING_CLAIM: &str = "SIBLING-DISJOINT-CLAIM-1913";

/// Planned declarations are globs, so selection has to be the exact
/// intersection of the lesson's scope glob with the declared glob — never a
/// string prefix. A wildcard declaration selects lessons whose scope really
/// overlaps it, and a same-prefix declaration that denotes a different subtree
/// selects nothing.
#[test]
fn cad1913_planned_wildcards_select_only_their_own_intersection() {
    let fixture = Fixture::new();
    write::set_fields(
        &fixture.pm,
        &[ISSUE.to_string()],
        &[format!("paths={WILDCARD_SUBTREE},{WILDCARD_SIBLING}")],
        "cad1913-acceptance",
        None,
    )
    .expect("declare the wildcard planned paths");

    // Overlaps the subtree declaration: a concrete file inside it, and a glob
    // whose files all lie inside it.
    accept_lesson(
        &fixture.pm,
        PROJECT,
        OVERLAP_FILE_SLUG,
        OVERLAP_FILE_CLAIM,
        crate::memory::Scope {
            paths: vec!["src/cad1913/planned.rs".to_string()],
            ..Default::default()
        },
    );
    accept_lesson(
        &fixture.pm,
        PROJECT,
        OVERLAP_GLOB_SLUG,
        OVERLAP_GLOB_CLAIM,
        crate::memory::Scope {
            paths: vec!["src/cad1913/*.rs".to_string()],
            ..Default::default()
        },
    );
    // Same textual prefix as the subtree declaration, different subtree.
    accept_lesson(
        &fixture.pm,
        PROJECT,
        PREFIX_SLUG,
        PREFIX_CLAIM,
        crate::memory::Scope {
            paths: vec!["src/cad1913-other/**".to_string()],
            ..Default::default()
        },
    );
    // Nested under the sibling declaration's prefix, but a single `*` does not
    // cross the segment boundary, so the declared set does not reach it.
    accept_lesson(
        &fixture.pm,
        PROJECT,
        SIBLING_SLUG,
        SIBLING_CLAIM,
        crate::memory::Scope {
            paths: vec!["src/other1913/deep/*.rs".to_string()],
            ..Default::default()
        },
    );

    let seam = issue_seam(&fixture);
    let (manifest, text) = (seam.manifest, seam.text);
    let delivery = delivered(&manifest, &text);

    let mut selected = lesson_ids(&manifest);
    selected.sort();
    assert_eq!(
        selected,
        vec![OVERLAP_FILE_SLUG.to_string(), OVERLAP_GLOB_SLUG.to_string()],
        "only lessons whose scope truly overlaps a declared glob may be selected\n{delivery}"
    );
    assert!(delivery.contains(OVERLAP_FILE_CLAIM), "{delivery}");
    assert!(delivery.contains(OVERLAP_GLOB_CLAIM), "{delivery}");

    // A prefix collision and a single-segment miss are both disjoint.
    assert_absent(
        &delivery,
        PREFIX_CLAIM,
        "a lesson scoped to a same-prefix but different subtree",
    );
    assert_absent(&delivery, PREFIX_SLUG, "a same-prefix disjoint lesson id");
    assert_absent(
        &delivery,
        SIBLING_CLAIM,
        "a lesson nested below a single-segment declaration",
    );
    assert_absent(
        &delivery,
        SIBLING_SLUG,
        "a single-segment disjoint lesson id",
    );
}

// ── Preserved axes, advisory condition, bounded overflow ────────

const ROLE_SLUG: &str = "alpha-role-qa-lesson";
const ROLE_CLAIM: &str = "ROLE-QA-CLAIM-1913";
const PROVIDER_OTHER_SLUG: &str = "alpha-provider-other-lesson";
const PROVIDER_OTHER_CLAIM: &str = "PROVIDER-OTHER-CLAIM-1913";
const PROVIDER_FAKE_SLUG: &str = "alpha-provider-fake-lesson";
const PROVIDER_FAKE_CLAIM: &str = "PROVIDER-FAKE-CLAIM-1913";
const CONDITION_SLUG: &str = "alpha-condition-lesson";
const CONDITION_CLAIM: &str = "CONDITION-CLAIM-1913";
const CONDITION_CHECK: &str = "cad1913-check";

/// The planned-path axis is additive: the existing role and provider axes keep
/// their meaning, and a declared verification condition is shown as advisory,
/// never as a verification.
#[test]
fn cad1913_role_provider_and_condition_axes_are_preserved() {
    let fixture = Fixture::new();
    // Role-scoped: withheld from a dev target, delivered to a qa target.
    accept_lesson_with(
        &fixture.pm,
        PROJECT,
        ROLE_SLUG,
        ROLE_CLAIM,
        planned_scope(),
        crate::memory::policy::ProposalOptions {
            audience: crate::memory::policy::Audience::Role,
            roles: vec!["qa".to_string()],
            ..Default::default()
        },
    );
    // Provider-only scopes: union semantics mean the provider is the only axis,
    // so only the matching provider selects them.
    accept_lesson(
        &fixture.pm,
        PROJECT,
        PROVIDER_OTHER_SLUG,
        PROVIDER_OTHER_CLAIM,
        crate::memory::Scope {
            providers: vec!["claude".to_string()],
            ..Default::default()
        },
    );
    accept_lesson(
        &fixture.pm,
        PROJECT,
        PROVIDER_FAKE_SLUG,
        PROVIDER_FAKE_CLAIM,
        crate::memory::Scope {
            providers: vec!["fake".to_string()],
            ..Default::default()
        },
    );
    // A declared check condition is data, never an execution or a result.
    accept_lesson_with(
        &fixture.pm,
        PROJECT,
        CONDITION_SLUG,
        CONDITION_CLAIM,
        planned_scope(),
        crate::memory::policy::ProposalOptions {
            verification_condition: Some(crate::memory::policy::VerificationCondition {
                check_id: CONDITION_CHECK.to_string(),
                source_revision: None,
                target_revision: None,
                wiki_target_revision: None,
            }),
            ..Default::default()
        },
    );

    let dev_seam = issue_seam(&fixture);
    let (dev_manifest, dev_text) = (dev_seam.manifest, dev_seam.text);
    let dev = delivered(&dev_manifest, &dev_text);
    assert!(
        !dev.contains(ROLE_CLAIM),
        "a qa-scoped lesson must not be delivered to a dev target\n{dev}"
    );
    assert!(
        dev.contains(PROVIDER_FAKE_CLAIM),
        "a lesson scoped to the target provider must be delivered\n{dev}"
    );
    assert!(
        !dev.contains(PROVIDER_OTHER_CLAIM),
        "a provider-only scope must not match another provider\n{dev}"
    );
    assert!(
        dev.contains(CONDITION_CLAIM),
        "the conditioned lesson must be delivered\n{dev}"
    );
    assert!(
        dev.contains(CONDITION_CHECK),
        "the declared condition must be shown\n{dev}"
    );
    assert!(
        dev.contains("advisory"),
        "a declared condition is advisory, never a verification\n{dev}"
    );
    assert!(
        !dev.contains("verified 20"),
        "an unverified lesson must not gain a verified label from its condition\n{dev}"
    );

    let qa_seam = issue_seam_for(&fixture, Some("qa"), "fake");
    let (qa_manifest, qa_text) = (qa_seam.manifest, qa_seam.text);
    let qa = delivered(&qa_manifest, &qa_text);
    assert!(
        qa.contains(ROLE_CLAIM),
        "a qa-scoped lesson must be delivered to a qa target\n{qa}"
    );
}

/// The lesson injection stays bounded: more matches than the cap are omitted
/// from the source, the omitted lesson is named in a bounded gap, and the
/// mandatory task body and required policy are delivered whole.
#[test]
fn cad1913_lesson_overflow_is_bounded_and_names_the_omitted_lesson() {
    let fixture = Fixture::new();
    let mut slugs = Vec::new();
    for index in 0..13 {
        let slug = format!("alpha-overflow-{index:02}");
        accept_lesson(
            &fixture.pm,
            PROJECT,
            &slug,
            &format!("OVERFLOW-CLAIM-{index:02}"),
            crate::memory::Scope {
                project: true,
                ..Default::default()
            },
        );
        slugs.push(slug);
    }

    let seam = issue_seam(&fixture);
    let (manifest, text, body) = (seam.manifest, seam.text, seam.body);
    let ids = lesson_ids(&manifest);
    assert_eq!(
        ids.len(),
        12,
        "the lesson injection cap must bound the delivered source: {ids:?}"
    );
    let omitted = slugs.last().expect("the fixture has thirteen lessons");
    assert!(
        !ids.contains(omitted),
        "the lesson beyond the cap must not be injected: {ids:?}"
    );
    let gap = manifest
        .withheld
        .iter()
        .find(|gap| gap.id == "native:lessons-omitted")
        .expect("an omitted lesson must be reported as a bounded gap");
    assert_eq!(gap.state, "overflow");
    assert!(
        gap.lessons.iter().any(|note| &note.id == omitted),
        "the omitted lesson must be named in its gap: {:?}",
        gap.lessons
    );

    // The turn delivers the seam's own body plus the appended context, joined
    // exactly as the plan accounts for it (`body_bytes + 2 + text.len()`).
    // Mandatory material is never displaced by the optional cap.
    assert!(
        !body.is_empty(),
        "the seam must prepare the body this turn sends"
    );
    let combined = format!("{body}\n\n{text}");
    assert!(
        combined.contains("The selected lesson is delivered inside its own project only."),
        "the mandatory acceptance criterion must survive the optional lesson cap"
    );
    assert!(
        combined.contains("The required policy body is present."),
        "the required policy document must be delivered whole"
    );
    assert!(
        combined.len() <= crate::context_pack::MAX_COMBINED_BYTES,
        "the combined body plus appended context must stay under the cap: {} bytes",
        combined.len()
    );
    assert!(
        !combined.contains("OVERFLOW-CLAIM-12"),
        "an omitted lesson's claim must not reach the delivered payload"
    );
}
