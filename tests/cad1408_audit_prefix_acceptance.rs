//! CAD-1408 independent acceptance: exercise the exported audit path, not a mock parser.
//! Authored independently of the src/audit.rs implementer; do not weaken this check.

use cadence_agent::audit::verdicts_check;
use serde_json::Value;
use std::path::Path;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER_HEAD: &str = "0123456789abcdef0123456789abcdef01234568";
const PR: u64 = 487;
const NAME: &str = "20261012-130000-cad1408-verdict.md";

fn note(issue: &str, title_result: &str, section_result: &str, pr: u64, head: &str) -> String {
    format!(
        "# Verdict: {issue} Standards review — {title_result}\n\
         > Issue: {issue}\n\
         > From: Reviewer.One (independent acceptance fixture)\n\n\
         ## Verdict\n\
         {section_result} — PR #{pr}, head {head}\n\n\
         Risk: human (7)\n"
    )
}

fn audit(dir: &Path, text: &str, issue: &str) -> Value {
    std::fs::write(dir.join(NAME), text).expect("write isolated verdict note");
    let result = verdicts_check(Some(dir), issue, PR, HEAD).expect("real audit reads temp notes");
    assert_eq!(result["issue"], issue);
    assert_eq!(result["pr"], PR);
    assert_eq!(result["head"], HEAD);
    result
}

fn assert_bound(result: &Value, outcome: &str, title: &str, section: &str) {
    let verdicts = result["verdicts"].as_array().expect("verdicts array");
    assert_eq!(verdicts.len(), 1, "exactly one bound note: {result}");
    let verdict = &verdicts[0];
    assert_eq!(verdict["name"], NAME);
    assert_eq!(verdict["stamp"], "20261012130000");
    assert_eq!(verdict["reviewer"], "reviewer.one");
    assert_eq!(
        verdict["from"],
        "Reviewer.One (independent acceptance fixture)"
    );
    assert_eq!(verdict["kind"], "standards");
    assert_eq!(verdict["risk"], "human");
    assert_eq!(verdict["head"], HEAD);
    assert_eq!(verdict["outcome"], outcome);
    assert_eq!(verdict["title_outcome"], title);
    assert_eq!(verdict["section_outcome"], section);
    assert_eq!(verdict["inline_outcome"], "");
    assert!(result["skipped"].as_array().unwrap().is_empty());
}

#[test]
fn project_prefix_verdicts_preserve_binding_and_refuse_unbound_passes() {
    let notes = tempfile::Builder::new()
        .prefix("cad1408-audit-")
        .tempdir_in("/tmp")
        .expect("isolated notes directory");
    let dir = notes.path();

    // Valid CAD remains valid; AOS and the registered prefix grammar's bounds
    // must work without a registry, daemon, renamed ticket or approval record.
    for issue in ["CAD-123", "AOS-225", "A1-225", "A-225", "A1B2C3D4-225"] {
        let result = audit(dir, &note(issue, "pass", "pass", PR, HEAD), issue);
        assert_bound(&result, "pass", "pass", "pass");
    }

    // A malformed header must not bind even when the requested issue matches
    // the entire malformed spelling or a valid-looking embedded substring.
    for (header, requested) in [
        ("aos-225", "aos-225"),
        ("aos-225", "AOS-225"),
        ("ABCDEFGHI-225", "ABCDEFGHI-225"),
        ("ABCDEFGHI-225", "BCDEFGHI-225"),
        ("xAOS-225", "AOS-225"),
        ("AOS-225x", "AOS-225"),
        ("AOS-225_suffix", "AOS-225"),
        ("_AOS-225", "AOS-225"),
        ("9AOS-225", "AOS-225"),
        ("AOS-", "AOS-"),
        ("AOS-２２５", "AOS-２２５"),
        ("xCAD-123", "CAD-123"),
        ("CAD-123_suffix", "CAD-123"),
        ("AOS-225", "AOS-226"),
        ("CAD-123", "AOS-225"),
    ] {
        let result = audit(dir, &note(header, "pass", "pass", PR, HEAD), requested);
        assert!(
            result["verdicts"].as_array().unwrap().is_empty(),
            "malformed or wrong issue {header:?} must not bind {requested:?}: {result}"
        );
    }

    for issue in ["CAD-123", "AOS-225"] {
        // Full-head and PR mismatches are surfaced as refused notes, not PASS.
        for (pr, head, reason) in [
            (PR, OTHER_HEAD, "not the full head"),
            (PR, &HEAD[..12], "not the full head"),
            (PR + 1, HEAD, "does not name PR #487"),
            (PR, "", "no `head <sha>` line"),
        ] {
            let result = audit(dir, &note(issue, "pass", "pass", pr, head), issue);
            assert!(
                result["verdicts"].as_array().unwrap().is_empty(),
                "{result}"
            );
            let skipped = result["skipped"].as_array().unwrap();
            assert_eq!(skipped.len(), 1, "{result}");
            assert_eq!(skipped[0]["name"], NAME);
            assert!(
                skipped[0]["reason"].as_str().unwrap().contains(reason),
                "{result}"
            );
        }

        let revise = note(issue, "revise", "revise", PR, HEAD);
        assert_bound(&audit(dir, &revise, issue), "revise", "revise", "revise");
        // A PASS title or a later inline PASS cannot launder a section REVISE.
        let conflict = note(issue, "pass", "revise", PR, HEAD);
        assert_bound(&audit(dir, &conflict, issue), "conflict", "pass", "revise");
        let result = audit(dir, &format!("{revise}\nVerdict: pass\n"), issue);
        assert_eq!(result["verdicts"].as_array().unwrap().len(), 1);
        assert_eq!(result["verdicts"][0]["outcome"], "conflict");
        assert_eq!(result["verdicts"][0]["section_outcome"], "revise");
        assert_eq!(result["verdicts"][0]["inline_outcome"], "pass");
        let result = audit(dir, &note(issue, "revise", "revise", PR, OTHER_HEAD), issue);
        assert!(
            result["verdicts"].as_array().unwrap().is_empty(),
            "{result}"
        );
        assert_eq!(result["skipped"].as_array().unwrap().len(), 1);
    }

    // The public API exposes only the first discovered header issue, not the
    // scanner's complete list: check first-issue binding, not private symbols.
    let text = note("AOS-225", "pass", "pass", PR, HEAD)
        .replace("> Issue: AOS-225", "> Issue: AOS-225 CAD-123 AOS-225");
    assert_bound(&audit(dir, &text, "AOS-225"), "pass", "pass", "pass");
    assert!(audit(dir, &text, "CAD-123")["verdicts"]
        .as_array()
        .unwrap()
        .is_empty());
}
