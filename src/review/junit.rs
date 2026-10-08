//! Structured JUnit evidence parsing for the review runner.

use std::path::Path;

use serde_json::{json, Value};

/// One testcase from a structured backend report. JUnit keeps names in the
/// same bare form accepted by libtest, which lets the isolated command use
/// the exact filter without guessing at package or binary prefixes.
#[derive(Clone, Debug)]
pub(super) struct TestCaseResult {
    name: String,
    outcome: &'static str,
    pub(super) duration_s: Option<f64>,
}

/// The smallest structured evidence needed by the review path: non-empty
/// testcase coverage, failure names, and per-test timings. Missing or empty
/// evidence is deliberately invalid so a successful process cannot launder a
/// zero-test or missing-report run into a pass.
#[derive(Clone, Debug)]
pub(super) struct TestRunSummary {
    pub(super) valid: bool,
    pub(super) test_count: u64,
    pub(super) passed: u64,
    pub(super) failed: u64,
    skipped: u64,
    pub(super) tests: Vec<TestCaseResult>,
    pub(super) failed_tests: Vec<String>,
    pub(super) reason: Option<String>,
}

impl TestRunSummary {
    fn invalid(reason: impl Into<String>) -> Self {
        Self {
            valid: false,
            test_count: 0,
            passed: 0,
            failed: 0,
            skipped: 0,
            tests: Vec::new(),
            failed_tests: Vec::new(),
            reason: Some(reason.into()),
        }
    }

    pub(super) fn to_json(&self) -> Value {
        let tests: Vec<Value> = self
            .tests
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "outcome": t.outcome,
                    "duration_s": t.duration_s,
                })
            })
            .collect();
        json!({
            "format": "junit",
            "valid": self.valid,
            "test_count": self.test_count,
            "passed": self.passed,
            "failed": self.failed,
            "skipped": self.skipped,
            "tests": tests,
            "failed_tests": self.failed_tests,
            "reason": self.reason,
        })
    }

    pub(super) fn executed_count(&self) -> u64 {
        self.passed + self.failed
    }
}

/// Parse the Jenkins XML emitted by cargo-nextest's configured JUnit
/// profile. This intentionally accepts only the small generated subset we
/// need instead of adding an XML dependency to the CLI; malformed, missing,
/// and empty documents remain invalid and therefore fail closed.
pub(super) fn parse_junit(text: &str) -> TestRunSummary {
    let mut root_seen = false;
    let mut tests = Vec::new();
    let mut current: Option<usize> = None;
    let mut stack: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    while let Some((end, raw)) = next_xml_tag(text, cursor) {
        cursor = end;
        let tag = raw.trim();
        if tag.starts_with("?") || tag.starts_with('!') {
            continue;
        }
        let closing = tag.starts_with('/');
        let self_closing = tag.ends_with('/');
        let name = xml_tag_name(tag);
        if name.is_empty() {
            return TestRunSummary::invalid("JUnit report has an empty tag name");
        }
        if closing {
            if stack.pop().as_deref() != Some(name) {
                return TestRunSummary::invalid("JUnit report has mismatched closing tags");
            }
        } else if !self_closing {
            stack.push(name.to_string());
        }
        if name == "testsuites" && !closing {
            root_seen = true;
        } else if name == "testcase" && !closing {
            let Some(raw_name) = xml_attr(tag, "name") else {
                return TestRunSummary::invalid("JUnit testcase has no name");
            };
            let name = xml_unescape(&raw_name);
            if name.is_empty() {
                return TestRunSummary::invalid("JUnit testcase has an empty name");
            }
            let duration_s = xml_attr(tag, "time").and_then(|v| v.parse::<f64>().ok());
            tests.push(TestCaseResult {
                name,
                outcome: "pass",
                duration_s,
            });
            current = Some(tests.len() - 1);
            if self_closing {
                current = None;
            }
        } else if !closing && matches!(name, "failure" | "error" | "flakyFailure") {
            if let Some(i) = current {
                tests[i].outcome = "fail";
            }
        } else if !closing && name == "skipped" {
            if let Some(i) = current {
                if tests[i].outcome == "pass" {
                    tests[i].outcome = "skipped";
                }
            }
        } else if closing && name == "testcase" {
            current = None;
        }
    }

    if !stack.is_empty() {
        return TestRunSummary::invalid("JUnit report ended before closing all tags");
    }
    if !root_seen {
        return TestRunSummary::invalid("JUnit report has no <testsuites> root");
    }
    if tests.is_empty() {
        return TestRunSummary::invalid("JUnit report contained zero testcases");
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut failed_tests = Vec::new();
    for test in &tests {
        match test.outcome {
            "pass" => passed += 1,
            "fail" => {
                failed += 1;
                failed_tests.push(test.name.clone());
            }
            "skipped" => skipped += 1,
            _ => {}
        }
    }
    failed_tests.sort();
    failed_tests.dedup();
    let reason =
        (passed + failed == 0).then(|| "JUnit report contained no executed testcases".to_string());
    TestRunSummary {
        valid: true,
        test_count: tests.len() as u64,
        passed,
        failed,
        skipped,
        tests,
        failed_tests,
        reason,
    }
}

pub(super) fn parse_junit_file(path: &Path) -> TestRunSummary {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_junit(&text),
        Err(e) => {
            TestRunSummary::invalid(format!("JUnit report {} unavailable: {e}", path.display()))
        }
    }
}

fn next_xml_tag(text: &str, from: usize) -> Option<(usize, &str)> {
    let start = from + text[from..].find('<')?;
    let end = start + text[start..].find('>')? + 1;
    Some((end, &text[start + 1..end - 1]))
}

fn xml_tag_name(tag: &str) -> &str {
    tag.trim_start_matches('/')
        .trim_end_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
}

fn xml_attr(tag: &str, wanted: &str) -> Option<String> {
    let mut rest = tag.trim_start_matches('/').trim();
    let name = xml_tag_name(rest);
    rest = rest.get(name.len()..)?.trim_start();
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let key = &rest[..key_end];
        rest = rest[key_end..].trim_start();
        if !rest.starts_with('=') {
            rest = rest
                .find(char::is_whitespace)
                .map(|i| rest[i..].trim_start())
                .unwrap_or("");
            continue;
        }
        rest = rest[1..].trim_start();
        let quote = rest.chars().next()?;
        if quote != '\'' && quote != '"' {
            return None;
        }
        let value = &rest[quote.len_utf8()..];
        let end = value.find(quote)?;
        let parsed = value[..end].to_string();
        rest = rest[quote.len_utf8() + end + quote.len_utf8()..].trim_start();
        if key == wanted {
            return Some(parsed);
        }
    }
    None
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
