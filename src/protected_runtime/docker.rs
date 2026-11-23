//! Deterministic Docker Engine API client over the root-controlled Unix
//! socket. Only the fixed endpoints this profile needs are exposed: create,
//! start, inspect, stop, remove and list owned containers. No shell-out, no
//! arbitrary Docker flag, no `docker exec`, no image pull, no network or
//! management endpoint.
//!
//! Requests are built by [`super::spec`] from the root profile only. A lost
//! create/start acknowledgement is reconciled by exact owned name/labels and
//! live container state — never by starting a replacement.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};

const CONNECT_BUDGET: Duration = Duration::from_secs(5);
const IO_BUDGET: Duration = Duration::from_secs(30);
const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const API_VERSION: &str = "v1.44";

pub(crate) struct DockerClient {
    socket: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CreateResult {
    pub(crate) id: String,
}

impl DockerClient {
    pub(crate) fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
        }
    }

    fn call(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<(u16, Vec<u8>)> {
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|e| Error::unknown(format!("docker engine socket unavailable: {e}")))?;
        stream
            .set_read_timeout(Some(IO_BUDGET))
            .map_err(|_| Error::unknown("docker engine read deadline unavailable"))?;
        stream
            .set_write_timeout(Some(IO_BUDGET))
            .map_err(|_| Error::unknown("docker engine write deadline unavailable"))?;
        let the_body = body.unwrap_or(&[]);
        let mut request = Vec::with_capacity(256 + the_body.len());
        request.extend_from_slice(
            format!(
                "{method} /{API_VERSION}{path} HTTP/1.1\r\n\
                 Host: docker\r\n\
                 Accept: application/json\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                the_body.len()
            )
            .as_bytes(),
        );
        request.extend_from_slice(the_body);
        stream
            .write_all(&request)
            .map_err(|e| Error::unknown(format!("docker engine write failed: {e}")))?;
        let mut raw = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&chunk[..n]);
                    if raw.len() > MAX_RESPONSE {
                        return Err(Error::unknown("docker engine response exceeds the bound"));
                    }
                }
                Err(e) => {
                    return Err(Error::unknown(format!(
                        "docker engine response failed: {e}"
                    )))
                }
            }
        }
        if raw.is_empty() {
            return Err(Error::unknown("docker engine returned no response"));
        }
        parse_response(&raw)
    }

    pub(crate) fn ping(&self) -> Result<()> {
        let (status, _) = self.call("GET", "/_ping", None)?;
        if status != 200 {
            return Err(Error::unknown("docker engine ping failed"));
        }
        Ok(())
    }

    /// `GET /info` — the fixed v1.44 engine facts. Only the trusted
    /// root-controlled data root (`DockerRootDir`) is consumed; the caller
    /// opens that directory itself rather than trusting a string path.
    pub(crate) fn info(&self) -> Result<serde_json::Value> {
        let (status, payload) = self.call("GET", "/info", None)?;
        if status != 200 {
            return Err(docker_error("docker info", status, &payload));
        }
        serde_json::from_slice(&payload)
            .map_err(|_| Error::unknown("docker info response is not JSON"))
    }

    /// `POST /containers/create?name=<name>` — the name is deterministic and
    /// owned; a `409` means the exact object already exists and is reported to
    /// the caller for reconciliation, never replaced.
    pub(crate) fn create(&self, name: &str, body: &[u8]) -> Result<CreateResult> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(Error::internal(
                "container name is not a bounded owned name",
            ));
        }
        let path = format!("/containers/create?name={}", encode(name));
        let (status, payload) = self.call("POST", &path, Some(body))?;
        if status != 201 && status != 200 {
            return Err(docker_error("container create", status, &payload));
        }
        let value: serde_json::Value = serde_json::from_slice(&payload)
            .map_err(|_| Error::unknown("docker create response is not JSON"))?;
        let id = value
            .get("Id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::unknown("docker create response has no Id"))?;
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::unknown("docker container id is malformed"));
        }
        Ok(CreateResult { id: id.to_string() })
    }

    pub(crate) fn start(&self, id: &str) -> Result<()> {
        let (status, payload) =
            self.call("POST", &format!("/containers/{}/start", encode(id)), None)?;
        if status == 204 || status == 304 || status == 200 {
            return Ok(());
        }
        Err(docker_error("container start", status, &payload))
    }

    pub(crate) fn inspect(&self, id: &str) -> Result<serde_json::Value> {
        let (status, payload) =
            self.call("GET", &format!("/containers/{}/json", encode(id)), None)?;
        if status != 200 {
            return Err(docker_error("container inspect", status, &payload));
        }
        serde_json::from_slice(&payload)
            .map_err(|_| Error::unknown("docker inspect response is not JSON"))
    }

    pub(crate) fn inspect_by_name(&self, name: &str) -> Result<Option<serde_json::Value>> {
        match self.inspect(name) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == "rejected" => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `POST /containers/<id>/attach` with the fixed hijack options. Only the
    /// owner calls this, only for a runtime it already owns and started. The
    /// returned stream is positioned immediately after the `101 UPGRADED`
    /// response head, so no attach byte can be lost to buffering.
    pub(crate) fn attach(&self, id: &str) -> Result<UnixStream> {
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|e| Error::unknown(format!("docker engine socket unavailable: {e}")))?;
        stream
            .set_read_timeout(Some(CONNECT_BUDGET))
            .map_err(|_| Error::unknown("docker attach deadline unavailable"))?;
        stream
            .set_write_timeout(Some(CONNECT_BUDGET))
            .map_err(|_| Error::unknown("docker attach deadline unavailable"))?;
        let request = format!(
            "POST /{API_VERSION}/containers/{}/attach?stream=1&stdin=1&stdout=1&stderr=1 HTTP/1.1\r\n\
             Host: docker\r\n\
             Connection: Upgrade\r\n\
             Upgrade: tcp\r\n\
             Content-Length: 0\r\n\r\n",
            encode(id)
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| Error::unknown(format!("docker attach request failed: {e}")))?;
        // Read the response head one byte at a time so nothing of the raw
        // stream is consumed into a buffer we then drop.
        let mut head = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        loop {
            match stream.read(&mut byte) {
                Ok(0) => return Err(Error::unknown("docker attach closed before a response")),
                Ok(_) => {
                    head.push(byte[0]);
                    if head.len() > 8192 {
                        return Err(Error::unknown(
                            "docker attach response head exceeds the bound",
                        ));
                    }
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                Err(e) => {
                    return Err(Error::unknown(format!(
                        "docker attach response failed: {e}"
                    )))
                }
            }
        }
        let head = std::str::from_utf8(&head)
            .map_err(|_| Error::unknown("docker attach response head is not UTF-8"))?;
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| Error::unknown("docker attach status is malformed"))?;
        if status != 101 {
            return Err(Error::rejected(format!(
                "docker attach was not upgraded (status {status})"
            )));
        }
        stream
            .set_read_timeout(None)
            .map_err(|_| Error::unknown("docker attach read deadline cannot be cleared"))?;
        stream
            .set_write_timeout(Some(IO_BUDGET))
            .map_err(|_| Error::unknown("docker attach write deadline unavailable"))?;
        Ok(stream)
    }

    /// `GET /images/<ref>/json` — the pinned reference must resolve to a real
    /// image whose `RepoDigests` contains that exact reference.
    pub(crate) fn image_inspect(&self, reference: &str) -> Result<serde_json::Value> {
        let (status, payload) =
            self.call("GET", &format!("/images/{}/json", encode(reference)), None)?;
        if status != 200 {
            return Err(docker_error("image inspect", status, &payload));
        }
        serde_json::from_slice(&payload)
            .map_err(|_| Error::unknown("docker image inspect response is not JSON"))
    }

    pub(crate) fn stop(&self, id: &str) -> Result<()> {
        let (status, payload) = self.call(
            "POST",
            &format!("/containers/{}/stop?t=5", encode(id)),
            None,
        )?;
        if status == 204 || status == 304 || status == 200 || status == 404 {
            return Ok(());
        }
        Err(docker_error("container stop", status, &payload))
    }

    pub(crate) fn remove(&self, id: &str) -> Result<()> {
        let (status, payload) = self.call(
            "DELETE",
            &format!("/containers/{}?force=0", encode(id)),
            None,
        )?;
        if status == 204 || status == 200 || status == 404 {
            return Ok(());
        }
        Err(docker_error("container remove", status, &payload))
    }

    /// Owned containers only: the exact owner label. Used for reconciliation,
    /// never for stopping anything the profile did not create.
    pub(crate) fn list_owned(&self, owner_label: &str) -> Result<Vec<serde_json::Value>> {
        let filters = serde_json::json!({"label": [owner_label]});
        let path = format!(
            "/containers/json?all=1&filters={}",
            encode(&filters.to_string())
        );
        let (status, payload) = self.call("GET", &path, None)?;
        if status != 200 {
            return Err(docker_error("container list", status, &payload));
        }
        let value: serde_json::Value = serde_json::from_slice(&payload)
            .map_err(|_| Error::unknown("docker list response is not JSON"))?;
        value
            .as_array()
            .cloned()
            .ok_or_else(|| Error::unknown("docker list response is not an array"))
    }
}

fn docker_error(what: &str, status: u16, payload: &[u8]) -> Error {
    // Docker's own message is bounded and non-secret; never echo a body.
    let message = serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "no message".to_string());
    let message: String = message.chars().take(200).collect();
    if status == 404 {
        return Error::rejected(format!("{what}: not found"));
    }
    Error::unknown(format!("{what}: docker status {status} ({message})"))
}

fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| Error::unknown("docker engine response has no header terminator"))?;
    let head = &raw[..split];
    let body = &raw[split + 4..];
    let head = std::str::from_utf8(head)
        .map_err(|_| Error::unknown("docker engine response header is not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| Error::unknown("docker engine response has no status line"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| Error::unknown("docker engine status is malformed"))?;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            if name == "content-length" {
                content_length = value.parse::<usize>().ok();
            } else if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
                chunked = true;
            }
        }
    }
    if chunked {
        return Ok((status, decode_chunked(body)?));
    }
    match content_length {
        Some(len) => {
            if body.len() < len {
                return Err(Error::unknown("docker engine response is truncated"));
            }
            Ok((status, body[..len].to_vec()))
        }
        None => Ok((status, body.to_vec())),
    }
}

fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| Error::unknown("chunked body is truncated"))?;
        let size_text = std::str::from_utf8(&body[..line_end])
            .map_err(|_| Error::unknown("chunk size is not UTF-8"))?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| Error::unknown("chunk size is malformed"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err(Error::unknown("chunked body is truncated"));
        }
        out.extend_from_slice(&body[..size]);
        if &body[size..size + 2] != b"\r\n" {
            return Err(Error::unknown("chunked body has no terminator"));
        }
        body = &body[size + 2..];
        if out.len() > MAX_RESPONSE {
            return Err(Error::unknown("chunked body exceeds the bound"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}";
        let (status, body) = parse_response(raw).unwrap();
        assert_eq!(status, 201);
        assert_eq!(body, b"{}");
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        let (status, body) = parse_response(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"{}");
    }

    #[test]
    fn encodes_identity_segments() {
        assert_eq!(
            encode("registry.example/runtime@sha256:aa"),
            "registry.example%2Fruntime%40sha256%3Aaa"
        );
        assert_eq!(encode("plain-name_1"), "plain-name_1");
    }

    #[test]
    fn rejects_truncated_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{}";
        assert!(parse_response(raw).is_err());
    }
}
