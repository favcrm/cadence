//! Hosted, exact-head assigned review management. The server owns the plan.

use super::{call, check_receipt, refuse, GateBinding, Map, RemoteTarget, Result, Value};

fn answer(body: &Value, binding: &GateBinding, state: &str) -> Result<()> {
    if body["protocol"].as_str() != Some("review-assignments.v1")
        || body["state"].as_str() != Some(state)
    {
        return Err(refuse(
            "review assignments returned an unsupported protocol or state",
        ));
    }
    binding.echoes(body, "review_assignments", true)?;
    if state == "ok" {
        let plan = &body["plan"];
        if !plan.is_object()
            || plan["policy_revision"].as_u64().is_none()
            || plan["override_revision"].as_u64().is_none()
            || !plan["slots"].is_array()
            || !body["assignments"].is_array()
            || !(body
                .get("override")
                .is_some_and(|v| v.is_null() || v.is_object()))
        {
            return Err(refuse(
                "review assignments returned a malformed plan or assignments",
            ));
        }
        if !plan["slots"].as_array().is_some_and(|slots| {
            slots.iter().all(|slot| {
                slot["name"].as_str().is_some_and(|s| !s.is_empty())
                    && slot["profile_id"].as_str().is_some_and(|s| !s.is_empty())
                    && matches!(
                        slot["focus"].as_str(),
                        Some("standards" | "spec-security" | "general")
                    )
                    && matches!(slot["source"].as_str(), Some("project" | "task"))
            })
        }) {
            return Err(refuse("review assignments returned malformed plan slots"));
        }
    } else if !body
        .get("override")
        .is_some_and(|v| v.is_null() || v.is_object())
    {
        return Err(refuse("review assignments returned a malformed override"));
    }
    Ok(())
}

fn perform(
    binding: &GateBinding,
    change: Option<Value>,
    mut send: impl FnMut(&str, Map<String, Value>) -> Result<Value>,
) -> Result<Value> {
    let read = send("review_assignments", binding.arguments())?;
    answer(&read, binding, "ok")?;
    let Some(change) = change else {
        return Ok(read);
    };
    let change = change
        .as_object()
        .ok_or_else(|| refuse("invalid assignment action"))?;
    let action = change
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| refuse("missing assignment action"))?;
    let mut write = binding.arguments();
    for (key, value) in change {
        if write.insert(key.clone(), value.clone()).is_some() {
            return Err(refuse(
                "an assignment action cannot change the exact-head selector",
            ));
        }
    }
    let body = send("review_assignments_write", write)?;
    check_receipt("review_assignments_write", || {
        answer(
            &body,
            binding,
            if action == "override" {
                "recorded"
            } else {
                "ok"
            },
        )?;
        if action == "override" {
            let recorded = &body["override"];
            let requested_remove = change
                .get("remove")
                .and_then(Value::as_array)
                .ok_or_else(|| refuse("override has no removal list"))?;
            if recorded.is_null() {
                if !requested_remove.is_empty() {
                    return Err(refuse(
                        "override receipt did not record the requested removals",
                    ));
                }
            } else {
                let mut expected = requested_remove.clone();
                let mut actual = recorded["remove"]
                    .as_array()
                    .cloned()
                    .ok_or_else(|| refuse("override receipt has no removal list"))?;
                expected.sort_by_key(Value::to_string);
                actual.sort_by_key(Value::to_string);
                if actual != expected
                    || recorded.get("policy_revision") != change.get("expected_policy_revision")
                {
                    return Err(refuse(
                        "override receipt does not match the requested policy and removals",
                    ));
                }
            }
        }
        Ok(body)
    })
}

/// Read or change assignments, after proving support for this exact tuple.
pub fn execute(
    target: &RemoteTarget,
    wake_timeout: u64,
    binding: &GateBinding,
    change: Option<Value>,
) -> Result<Value> {
    perform(binding, change, |verb, args| {
        call(target, wake_timeout, verb, args)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn server_fixture_probe_gates_the_write() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/hosted-cadence-assigned-reviews.fixture.json"
        ))
        .expect("server fixture");
        let binding = GateBinding::new(
            "app",
            "org/app",
            "APP-1",
            7,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("binding");
        for bad in [
            Value::Null,
            json!({"state":"ok"}),
            json!({"protocol":"review-assignments.v1","state":"ok","project":"other"}),
        ] {
            let mut calls = Vec::new();
            assert!(perform(
                &binding,
                Some(json!({"action":"dispatch"})),
                |verb, args| {
                    calls.push((verb.to_string(), args));
                    Ok(bad.clone())
                }
            )
            .is_err());
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, fixture["inspect"]["verb"]);
            assert_eq!(
                Value::Object(calls[0].1.clone()),
                fixture["inspect"]["arguments"]
            );
        }
        let mut calls = Vec::new();
        perform(
            &binding,
            Some(json!({"action":"dispatch"})),
            |verb, args| {
                calls.push((verb.to_string(), args));
                Ok(fixture["reply"].clone())
            },
        )
        .expect("valid fixture");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, fixture["dispatch"]["verb"]);
        assert_eq!(
            Value::Object(calls[1].1.clone()),
            fixture["dispatch"]["arguments"]
        );
    }
}
