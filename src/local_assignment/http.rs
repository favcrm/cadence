use super::*;
use std::time::Instant;

pub struct BoardHttp {
    endpoint: String,
    org_id: String,
    /// Saved-login authority captured at open; absent for an explicit bearer.
    saved_target: Option<RemoteTarget>,
    agent: ureq::Agent,
    bearer: String,
    ledger: PathBuf,
    subject_id: Option<String>,
    issuer: Option<String>,
    request_guard: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl BoardHttp {
    pub fn from_parts(
        endpoint: String,
        org_id: String,
        bearer: String,
        ledger: PathBuf,
        subject_id: Option<String>,
        issuer: Option<String>,
    ) -> Self {
        let builder = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None);
        let agent = crate::issuer_transport::agent(builder);
        Self {
            endpoint,
            org_id,
            saved_target: None,
            agent,
            bearer,
            ledger,
            subject_id,
            issuer,
            request_guard: None,
        }
    }

    pub fn open(target: &RemoteTarget) -> Result<Self> {
        let ledger = crate::remote_auth::auth_dir(None).map_err(|_| {
            Error::rejected(
                "remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME",
            )
        })?;
        Self::open_with(&ledger, target, remote_cli::cli_material(&ledger, target)?)
    }

    /// Open from the saved login, sharing the caller's total deadline for
    /// renewal. The pooled agent is the same one `open` builds; each later
    /// send applies only the time still left.
    pub fn open_until(target: &RemoteTarget, deadline: Instant) -> Result<Self> {
        let ledger = crate::remote_auth::auth_dir(None).map_err(|_| {
            Error::rejected(
                "remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME",
            )
        })?;
        Self::open_with(
            &ledger,
            target,
            remote_cli::cli_material_until(&ledger, target, deadline)?,
        )
    }

    fn open_with(ledger: &Path, target: &RemoteTarget, material: CliMaterial) -> Result<Self> {
        let CliMaterial {
            access_token,
            subject_id,
            issuer,
        } = material;
        if !valid_hct(&access_token) || !hosted_id(&target.org_id) {
            return Err(Error::rejected("invalid remote credential"));
        }
        let mut client = Self::from_parts(
            target.endpoint.clone(),
            target.org_id.clone(),
            access_token,
            ledger.to_path_buf(),
            subject_id,
            issuer,
        );
        client.saved_target = Some(target.clone());
        Ok(client)
    }

    /// Reload the saved login for the org, audience, and member captured at
    /// open. A rotated token is used. A different member or issuer is refused.
    /// Renewal stays on `cli_material`, which keeps the stored scopes and does
    /// not select another org.
    fn reload_saved_login(&mut self) -> Result<()> {
        self.reload_saved_login_until(Instant::now() + Duration::from_secs(20))
    }

    fn reload_saved_login_until(&mut self, deadline: Instant) -> Result<()> {
        let Some(target) = self.saved_target.as_ref() else {
            return Ok(());
        };
        let material = remote_cli::cli_material_until(&self.ledger, target, deadline)?;
        if material.subject_id != self.subject_id || material.issuer != self.issuer {
            return Err(Error::rejected(
                "saved login changed member or issuer; this session kept the original identity and sent nothing",
            ));
        }
        if !valid_hct(&material.access_token) {
            return Err(Error::rejected("invalid remote credential"));
        }
        self.bearer = material.access_token;
        Ok(())
    }

    pub fn into_coordinator(self) -> Result<Coordinator<Self>> {
        let org_id = self.org_id.clone();
        let ledger = self.ledger.clone();
        let subject_id = self.subject_id.clone();
        let issuer = self.issuer.clone();
        let audience = self.endpoint.clone();
        Coordinator::open(self, org_id, ledger, subject_id, issuer, audience)
    }
    pub(super) fn set_request_guard(
        &mut self,
        guard: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        self.request_guard = Some(guard);
    }

    fn ensure_request_active(&self) -> Result<()> {
        if self.request_guard.as_ref().is_some_and(|active| !active()) {
            return Err(Error::busy_coded(
                "binding_cancelled",
                "workspace binding stopped or changed before the request",
            ));
        }
        Ok(())
    }

    /// Exact saved-login identity for daemon persistence; never resolve a
    /// later global org selection or fill a missing claimant pin.
    pub fn pinned_target(&self) -> Result<RemoteTarget> {
        let subject_id = self
            .subject_id
            .clone()
            .ok_or_else(|| Error::rejected("workspace member pin is missing"))?;
        let issuer = self
            .issuer
            .clone()
            .ok_or_else(|| Error::rejected("workspace issuer pin is missing"))?;
        Ok(RemoteTarget {
            org: self
                .saved_target
                .as_ref()
                .map(|target| target.org.clone())
                .unwrap_or_else(|| self.org_id.clone()),
            org_id: self.org_id.clone(),
            endpoint: self.endpoint.clone(),
            identity: Some(remote_cli::CliIdentityPin { subject_id, issuer }),
        })
    }

    pub fn events_socket(
        &mut self,
        after: u64,
        runner: &str,
    ) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>>
    {
        use tungstenite::client::IntoClientRequest;
        self.reload_saved_login()?;
        self.ensure_request_active()?;
        if !prefixed_id(runner, "lri_") {
            return Err(Error::rejected("runner identity is invalid"));
        }
        let origin = self
            .endpoint
            .strip_prefix("https://")
            .ok_or_else(|| Error::rejected("hosted events require the pinned HTTPS board"))?;
        let url = format!(
            "wss://{}{}/events?after={after}&runner_id={runner}",
            origin.trim_end_matches('/'),
            assignment_prefix(&self.org_id)
        );
        let mut request = url
            .into_client_request()
            .map_err(|_| Error::rejected("event endpoint is invalid"))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.bearer)
                .parse()
                .map_err(|_| Error::rejected("event credential is invalid"))?,
        );
        let mut config = tungstenite::protocol::WebSocketConfig::default();
        config.max_message_size = Some(16 * 1024);
        config.max_frame_size = Some(16 * 1024);
        config.max_write_buffer_size = 64 * 1024;
        config.write_buffer_size = 0;
        crate::issuer_transport::websocket(request, Some(config), Duration::from_secs(5))
    }
}

impl BoardHttp {
    /// One assignment request whose renewal and HTTP share `deadline`.
    pub fn send_until(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        deadline: Instant,
    ) -> Result<Reply> {
        if Instant::now() >= deadline {
            return Err(Error::busy_coded(
                "timeout",
                "hosted report deadline elapsed before the request",
            ));
        }
        self.reload_saved_login_until(deadline)?;
        self.send_prepared(method, path, body, Some(deadline))
    }

    fn send_prepared(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        deadline: Option<Instant>,
    ) -> Result<Reply> {
        // Credential renewal may have blocked while a configuration changed.
        // Re-check at the transport boundary, not only before renewal.
        self.ensure_request_active()?;
        if !valid_hct(&self.bearer) {
            return Err(Error::rejected("invalid remote credential"));
        }
        let prefix = assignment_prefix(&self.org_id);
        let directory = format!("/__platform/hosted-cadence/{}/local-agents", self.org_id);
        let route = path.split_once('?').map_or(path, |(route, _)| route);
        if !hosted_id(&self.org_id)
            || ![prefix.as_str(), directory.as_str()].iter().any(|prefix| {
                route == *prefix
                    || route
                        .strip_prefix(*prefix)
                        .is_some_and(|tail| tail.starts_with('/'))
            })
            || path.contains("..")
            || path.contains("tool-exchange")
            || path.contains("/presence")
        {
            return Err(Error::invalid(
                "cross_workspace",
                "refusing to send an assignment request outside this workspace",
            ));
        }
        let url = format!("{}{path}", self.endpoint.trim_end_matches('/'));
        let authorization = format!("Bearer {}", self.bearer);
        let timeout = deadline.map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1))
        });
        let response = match method {
            "GET" => {
                let request = self.agent.get(url).header("Authorization", authorization);
                match timeout {
                    Some(timeout) => request
                        .config()
                        .timeout_global(Some(timeout))
                        .build()
                        .call(),
                    None => request.call(),
                }
            }
            "POST" => {
                let request = self.agent.post(url).header("Authorization", authorization);
                match timeout {
                    Some(timeout) => request
                        .config()
                        .timeout_global(Some(timeout))
                        .build()
                        .send_json(body.unwrap_or(&json!({}))),
                    None => request.send_json(body.unwrap_or(&json!({}))),
                }
            }
            _ => return Err(Error::rejected("unsupported coordination method")),
        }
        .map_err(|_| Error::rejected("remote request failed"))?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response
            .into_body()
            .as_reader()
            .take(MAX_BODY + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BODY {
            return Err(Error::rejected("coordination response is too large"));
        }
        if (300..400).contains(&status) {
            return Err(Error::invalid(
                "redirect",
                "remote redirect refused; nothing was followed and nothing ran locally",
            ));
        }
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| Error::rejected("coordination response was not JSON"))?
        };
        Ok(Reply { status, body })
    }
}

impl CoordHttp for BoardHttp {
    fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        self.reload_saved_login()?;
        self.send_prepared(method, path, body, None)
    }
}
