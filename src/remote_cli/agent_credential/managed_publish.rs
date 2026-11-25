use super::*;
use std::borrow::Cow;

const MAX_PACKET: usize = 512 * 1024;

fn invalid() -> Error {
    Error::invalid(
        "invalid_request",
        "invalid managed publication packet; nothing was sent",
    )
}

fn unknown() -> Error {
    Error::unknown("managed publication outcome is unknown; reconcile the existing request before retrying the identical packet")
}

fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Only read the repo used to verify the receipt. The tenant validates the
/// entire payload before any effect, and every original byte is sent unchanged.
fn packet_repo(packet: &[u8]) -> Result<Cow<'_, str>> {
    #[derive(serde::Deserialize)]
    struct Repo<'a> {
        #[serde(borrow)]
        repo: Cow<'a, str>,
    }
    if packet.len() > MAX_PACKET {
        return Err(invalid());
    }
    let Repo { repo } = serde_json::from_slice(packet).map_err(|_| invalid())?;
    let (owner, name) = repo.split_once('/').ok_or_else(invalid)?;
    if !owner
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || owner.len() > 39
        || !owner
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || name.is_empty()
        || name.len() > 100
        || matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(invalid());
    }
    Ok(repo)
}

fn receipt(value: &Value, repo: &str) -> bool {
    if !exact_object(value, &["ok", "data"]) || value["ok"] != true {
        return false;
    }
    let data = &value["data"];
    if !exact_object(data, &["pr", "pullId", "head", "duplicate", "url"]) {
        return false;
    }
    let Some(pr) = data["pr"]
        .as_u64()
        .filter(|n| (1..=999_999_999).contains(n))
    else {
        return false;
    };
    data["pullId"]
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.encode_utf16().count() <= 200)
        && data["head"].as_str().is_some_and(sha)
        && data["duplicate"].is_boolean()
        && data["url"].as_str() == Some(format!("https://github.com/{repo}/pull/{pr}").as_str())
}

/// Publish once using the pinned managed claim. The tenant decides writer eligibility.
pub fn managed_publish(target: &RemoteTarget, wake_timeout: u64, packet: &[u8]) -> Result<Value> {
    let repo = packet_repo(packet)?;
    if !target.endpoint.starts_with("https://")
        || target.endpoint.contains(['?', '#'])
        || !valid_id(&target.org_id)
    {
        return Err(Error::invalid(
            "managed_writer_required",
            "invalid pinned tenant destination; nothing was sent",
        ));
    }
    let session = bound_session();
    let slot = session
        .as_deref()
        .filter(|s| s.serves(target) && !s.declined())
        .ok_or_else(|| {
            Error::invalid(
                "managed_writer_required",
                "managed publication requires a bound hosted claim; nothing was sent",
            )
        })?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| reject("invalid wake timeout"))?;
    let dir = auth_dir(None).map_err(|_| reject("remote credential directory is unavailable"))?;
    // Mint through the existing claim exchange but never cache the resulting bearer.
    let cred = match mint(slot, target, &dir, deadline)? {
        Minted::Credential(cred) => cred,
        Minted::Keep(_) => {
            return Err(Error::invalid(
                "managed_writer_required",
                "the tenant cannot issue a managed agent credential; nothing was sent",
            ))
        }
    };
    if !cred.capabilities.iter().any(|c| c == "cli.write")
        || !valid_token(&cred.access_token, "hct_")
    {
        return Err(Error::invalid(
            "insufficient_scope",
            "managed agent credential lacks cli.write; nothing was sent",
        ));
    }
    let cfg = ureq::Agent::config_builder()
        .timeout_global(Some(request_timeout(deadline, CALL_TIMEOUT)?))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    publish_packet(
        &crate::issuer_transport::agent(cfg),
        target,
        &cred.access_token,
        packet,
        &repo,
    )
}

fn publish_packet(
    agent: &ureq::Agent,
    target: &RemoteTarget,
    bearer: &str,
    packet: &[u8],
    repo: &str,
) -> Result<Value> {
    let response = agent
        .post(format!(
            "{}/__platform/hosted-cadence/{}/repo-publish",
            target.endpoint, target.org_id
        ))
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .send(packet)
        .map_err(|_| unknown())?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unknown())?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(unknown());
    }
    let body: Option<Value> = serde_json::from_slice(&bytes).ok();
    if (200..300).contains(&status) {
        return body.filter(|v| receipt(v, repo)).ok_or_else(unknown);
    }
    if status >= 500 || status == 409 || (300..400).contains(&status) {
        return Err(unknown());
    }
    let code = refusal_fields(body.as_ref()).0;
    match code {
        Some(code)
            if !code.is_empty()
                && code.len() <= 80
                && code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') =>
        {
            Err(Error::invalid(
                "managed_publish_refused",
                format!("tenant refused managed publication ({code})"),
            ))
        }
        _ => Err(Error::invalid(
            "managed_publish_refused",
            format!("tenant refused managed publication (HTTP {status})"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn managed_publish_sends_exact_fixture_bytes_and_validates_the_receipt() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/hosted-cadence-managed-publish.fixture.json"
        ))
        .unwrap();
        let packet = format!(
            "{}\n",
            serde_json::to_string_pretty(&fixture["request"]).unwrap()
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let fixture_path = fixture["path"].as_str().unwrap().to_owned();
        let reply = fixture["receipt"].to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            assert_eq!(line, format!("POST {fixture_path} HTTP/1.1\r\n"));
            let mut length = 0;
            let mut authorized = false;
            loop {
                line.clear();
                assert_ne!(stream.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.trim_end().split_once(':').unwrap();
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                } else if key.eq_ignore_ascii_case("authorization") {
                    authorized = value.trim() == "Bearer fixture";
                }
            }
            assert!(authorized);
            let mut received = vec![0; length];
            stream.read_exact(&mut received).unwrap();
            write!(
                stream.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len(),
            ).unwrap();
            received
        });
        let target = RemoteTarget {
            org: "fixture".to_owned(),
            endpoint,
            org_id: "ws_26e7601c-d62f-4fc3-8b19-df2980d098d8".to_owned(),
            identity: None,
        };
        let repo = packet_repo(packet.as_bytes()).unwrap();
        let cfg = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(4)))
            .proxy(None);
        let agent = crate::issuer_transport::agent(cfg);
        let result = publish_packet(&agent, &target, "fixture", packet.as_bytes(), &repo).unwrap();
        assert_eq!(result, fixture["receipt"]);
        assert_eq!(server.join().unwrap(), packet.as_bytes());
        let mut wrong = fixture["receipt"].clone();
        wrong["data"]["url"] = json!("https://github.com/other/repo/pull/7");
        assert!(!receipt(&wrong, &repo));
        wrong = fixture["receipt"].clone();
        wrong["data"]["head"] = json!("short");
        assert!(!receipt(&wrong, &repo));
    }
}
