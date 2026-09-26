//! CAD-619: credential-free, bounded read projection. No platform mutations.
use super::*;

const ACCOUNT_URL: &str = "http://api.internal/v1/runtime/account";
const USAGE_URL: &str = "http://api.internal/v1/runtime/account/usage?limit=20";
const BODY_CAP: u64 = 64 * 1024;

pub(super) fn get(opts: &ServeOpts) -> HttpResp {
    let Some(board) = opts.public.as_ref() else {
        return json_response(json!({"configured": false, "manage_url": null,
            "account": null, "usage": [], "account_error": null, "usage_error": null}));
    };
    // This data surface exists only on the hosted AgenticOS trust root.
    // Other public issuers must not cause a call to a guessed platform.
    if board.issuer != "http://api.internal" {
        return json_response(json!({"configured": false, "manage_url": null,
            "account": null, "usage": [], "account_error": null, "usage_error": null}));
    }
    let account = fetch(ACCOUNT_URL).and_then(project_account);
    let usage = fetch(USAGE_URL).and_then(project_usage);
    json_response(json!({
        "configured": true,
        "manage_url": manage_url(&board.host),
        "account": account.as_ref().ok(),
        "usage": usage.as_ref().ok().cloned().unwrap_or_default(),
        "account_error": account.err().map(|_| "Account information is unavailable. Try Refresh."),
        "usage_error": usage.err().map(|_| "Recent usage is unavailable. Try Refresh."),
    }))
}

fn fetch(url: &str) -> std::result::Result<Value, ()> {
    // Fixed URLs, no redirects, no incoming headers/cookies/token forwarded.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent.get(url).call().map_err(|_| ())?;
    if response.status() != 200 {
        return Err(());
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(BODY_CAP)
        .read_to_vec()
        .map_err(|_| ())?;
    let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if envelope["ok"] != true {
        return Err(());
    }
    envelope.get("data").cloned().ok_or(())
}

fn text(value: &Value, cap: usize) -> std::result::Result<&str, ()> {
    let value = value.as_str().ok_or(())?;
    if value.len() > cap || value.chars().any(char::is_control) {
        return Err(());
    }
    Ok(value)
}

fn amount(value: &Value, signed: bool) -> std::result::Result<&str, ()> {
    let value = text(value, 18)?;
    let unsigned = if signed {
        value.strip_prefix('-').unwrap_or(value)
    } else {
        value
    };
    let (whole, fraction) = unsigned.split_once('.').ok_or(())?;
    if whole.is_empty()
        || whole.len() > 10
        || (whole.len() > 1 && whole.starts_with('0'))
        || fraction.len() != 6
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(());
    }
    Ok(value)
}

fn project_account(data: Value) -> std::result::Result<Value, ()> {
    let name = text(&data["company"]["name"], 200)?;
    let balance = &data["balance"];
    let amount = amount(&balance["amount"], false)?;
    if balance["currency"] != "HKD" {
        return Err(());
    }
    let low = balance["low"].as_bool().ok_or(())?;
    let zero = balance["zero"].as_bool().ok_or(())?;
    let plan = if data["plan"].is_null() {
        Value::Null
    } else {
        let name = text(&data["plan"]["name"], 200)?;
        let status = text(&data["plan"]["status"], 20)?;
        if !matches!(status, "inactive" | "active" | "past_due") {
            return Err(());
        }
        json!({"name": name, "status": status})
    };
    // Returned links and every unknown field are intentionally discarded.
    Ok(
        json!({"company": {"name": name}, "balance": {"amount": amount,
        "currency": "HKD", "low": low, "zero": zero}, "plan": plan}),
    )
}

fn project_usage(data: Value) -> std::result::Result<Vec<Value>, ()> {
    let rows = data["rows"].as_array().ok_or(())?;
    if rows.len() > 20 {
        return Err(());
    }
    rows.iter()
        .map(|row| {
            let date = text(&row["date"], 40)?;
            let description = text(&row["description"], 200)?;
            let amount = amount(&row["amount"], true)?;
            let kind = text(&row["kind"], 10)?;
            if !matches!(kind, "credit" | "debit" | "charge" | "grant") {
                return Err(());
            }
            Ok(json!({"date": date, "description": description, "amount": amount, "kind": kind}))
        })
        .collect()
}

fn manage_url(host: &str) -> Option<String> {
    let slug = host.strip_suffix(".cadencecloud.app")?;
    if slug.is_empty()
        || slug.len() > 63
        || !slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || slug.starts_with('-')
        || slug.ends_with('-')
    {
        return None;
    }
    Some(format!(
        "https://app-v2.agenticos.hk/account?company={slug}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cad619_fetch_refuses_redirects_and_oversized_bodies_without_credentials() {
        for redirect in [true, false] {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let url = format!("http://{}/account", server.server_addr());
            let thread = std::thread::spawn(move || {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.method().as_str(), "GET");
                assert_eq!(request.url(), "/account");
                assert!(!request.headers().iter().any(|h| {
                    ["Cookie", "Authorization", "X-Board-Session"]
                        .iter()
                        .any(|name| h.field.equiv(*name))
                }));
                let response = if redirect {
                    tiny_http::Response::from_string("redirect")
                        .with_status_code(302)
                        .with_header(
                            tiny_http::Header::from_bytes("Location", "http://127.0.0.1:1/secret")
                                .unwrap(),
                        )
                } else {
                    tiny_http::Response::from_string("x".repeat(BODY_CAP as usize + 1))
                };
                request.respond(response).unwrap();
            });
            assert!(fetch(&url).is_err());
            thread.join().unwrap();
        }
    }

    #[test]
    fn cad619_projection_discards_credentials_and_platform_action_links() {
        let projected = project_account(json!({"company":{"name":"Acme","id":"secret"},
            "balance":{"amount":"1.000000","currency":"HKD","low":true,"zero":false},
            "plan":null,"links":{"account":"javascript:alert(1)","topUp":"https://evil.test"},
            "token":"must-not-forward"}))
        .unwrap();
        assert!(projected.get("links").is_none());
        assert!(projected.get("token").is_none());
        assert!(projected["company"].get("id").is_none());
        for host in [
            "evil.test",
            "acme.cadencecloud.app.evil.test",
            "a.b.cadencecloud.app",
            "-bad.cadencecloud.app",
        ] {
            assert!(manage_url(host).is_none());
        }
        assert_eq!(
            manage_url("acme.cadencecloud.app").unwrap(),
            "https://app-v2.agenticos.hk/account?company=acme"
        );
    }

    #[test]
    fn cad619_projection_refuses_malformed_and_unbounded_usage() {
        let row = json!({"date":"2026-09-26","description":"Model usage","amount":"-0.123456","kind":"charge"});
        assert_eq!(
            project_usage(json!({"rows":[row.clone()]})).unwrap().len(),
            1
        );
        assert!(project_usage(json!({"rows":vec![row;21]})).is_err());
        for value in ["NaN", "1.00", "01.000000", "-1.000000", "1e6.000000"] {
            assert!(amount(&json!(value), false).is_err(), "{value}");
        }
    }
}
