//! CAD-536: `cadence doctor host` check `tailnet` — moved verbatim from src/doctor/host.rs.

use super::*;

/// CAD-509 — the tailnet sign-in proof run before a link is spent.
/// [`crate::tailnet_proof::host_refusals`] reads every host-side rung
/// (the LocalAPI socket, kernel networking, the operator user, TCP
/// forwarders, tailscaled's uid); a live board's `/api/meta` adds
/// `operator_latched`, which is the board's memory and no host read
/// can see. The remedy prints the whole chain in order — the operator
/// fixes it in one pass instead of spending a link per refusal.
pub(super) fn check_tailnet(scan: &Scan) -> Check {
    use crate::tailnet_proof::Check as T;
    let opts = crate::ui::persisted_opts(&scan.state_dir);
    let sharing = opts.tailscale.is_some();
    let board_port = opts.port.unwrap_or(3010);
    let mut refusals = crate::tailnet_proof::host_refusals(
        scan.tailscaled_socket.as_deref(),
        scan.uid,
        board_port,
    );
    // A forged probe at a running board names the rung it fails now —
    // `operator_latched` lives in the board's memory. The per-connection
    // rungs (`client_socket`, `socket_owner`) refuse this probe by
    // design and say nothing about a real one.
    let live = if sharing {
        live_tailnet_verdict(&opts)
    } else {
        None
    };
    let mut forgeable = false;
    if let Some((proven, check, why)) = &live {
        if *proven {
            forgeable = true;
        } else if let Some(c) = check {
            if *c < T::ClientSocket && !refusals.iter().any(|r| r.check == *c) {
                refusals.push(crate::tailnet_proof::Refusal {
                    check: *c,
                    why: why.clone(),
                });
            }
        }
    }
    refusals.sort_by_key(|r| r.check);
    refusals.dedup_by_key(|r| r.check);
    let names: Vec<&str> = refusals.iter().map(|r| r.check.as_str()).collect();
    let (level, detail) = if forgeable {
        (
            Level::Fail,
            "a forged loopback request was proven as the tailscale proxy — tailnet \
             identity headers are forgeable"
                .to_string(),
        )
    } else if !sharing && refusals.iter().all(|r| r.check == T::TailscaledSocket) {
        (
            Level::Ok,
            "tailscale sharing is off and no tailscaled answers — nothing to prove".to_string(),
        )
    } else if refusals.is_empty() {
        match (sharing, live.is_some()) {
            (true, true) => (
                Level::Ok,
                "sharing on; every host-side rung passes and the board is not latched".to_string(),
            ),
            (true, false) => (
                Level::Warn,
                "sharing on and the host side is clean, but the board does not answer — \
                 its operator latch cannot be read until it runs"
                    .to_string(),
            ),
            (false, _) => (
                Level::Ok,
                "tailscale sharing is off; the host-side proof would pass".to_string(),
            ),
        }
    } else {
        (
            if sharing { Level::Fail } else { Level::Warn },
            format!("a tailnet sign-in would refuse on: {}", names.join(", ")),
        )
    };
    let mut remedy = String::new();
    if level != Level::Ok {
        let mut steps: Vec<String> = refusals
            .iter()
            .map(|r| format!("{} — {}", r.check.as_str(), tailnet_remedy(r.check)))
            .collect();
        if forgeable {
            steps.push(
                "stop sharing until the proof is sound — `cadence ui tailscale stop`".to_string(),
            );
        } else {
            if sharing && live.is_none() {
                steps.push(
                    "the board is down — `cadence ui start` (its latch cannot be read \
                     while stopped)"
                        .to_string(),
                );
            }
            steps.push(if sharing {
                "then mint a fresh link — `cadence ui login --tailnet`".to_string()
            } else {
                "then `cadence ui tailscale start` and `cadence ui login --tailnet`".to_string()
            });
        }
        remedy = steps.join("\n");
    }
    check(
        "tailnet",
        level,
        json!({
            "sharing": sharing,
            "refusals": names,
            "board_answered": live.is_some(),
        }),
        json!("no refusal ahead of a tailnet sign-in"),
        detail,
        remedy,
    )
}

/// A forged probe at the running board's `/api/meta` — the tailnet
/// Host, a made-up login: `(proven, check, why)`. `operator_latched`
/// is the board's memory; only this sees it. `None` when sharing is
/// off or nothing answers on the board's port.
fn live_tailnet_verdict(
    opts: &crate::ui::UiOpts,
) -> Option<(bool, Option<crate::tailnet_proof::Check>, String)> {
    let ts = opts.tailscale.as_ref()?;
    let port = opts.port.unwrap_or(3010);
    let host = format!("{}:{}", ts.dns_name, ts.https_port);
    let (code, body) = crate::ui::http_get(
        "127.0.0.1",
        port,
        "/api/meta",
        &host,
        &["Tailscale-User-Login: doctor-probe@cadence.invalid"],
    )
    .ok()?;
    if code != 200 {
        return None;
    }
    let meta: Value = serde_json::from_str(&body).ok()?;
    let proof = &meta["tailnet_proof"];
    Some((
        proof["proven"].as_bool()?,
        crate::tailnet_proof::Check::named(proof["check"].as_str().unwrap_or_default()),
        proof["why"].as_str().unwrap_or_default().to_string(),
    ))
}

/// The operator's fix for one tailnet refusal — the command, not the
/// explanation (the refusal's `why` already carries that).
fn tailnet_remedy(check: crate::tailnet_proof::Check) -> &'static str {
    use crate::tailnet_proof::Check::*;
    match check {
        TailscaledSocket => {
            "start tailscaled — `sudo systemctl start tailscaled`, then `tailscale up`"
        }
        Localapi => "tailscaled must answer its LocalAPI — `sudo systemctl status tailscaled`",
        KernelNetworking => {
            "run tailscaled on kernel networking (a TUN device) — userspace networking \
             can never prove a peer"
        }
        NotOperatorUser => {
            "move the operator seat off the board's uid — `sudo tailscale set \
             --operator=root` (root already holds every power), or clear it with \
             `sudo tailscale set --operator=`"
        }
        OperatorLatched => {
            "restart the board so the latch clears — `cadence ui stop && cadence ui start`"
        }
        NoTcpForwarder => {
            "drop the TCP forwarder to the board's port — `tailscale serve status`, then \
             `tailscale serve --tcp=<port> off`"
        }
        ClientSocket | SocketOwner => {
            "the connection was not tailscaled's own — rerun doctor; if it persists, \
             restart tailscaled"
        }
        ForeignUid => {
            "run tailscaled as its own user (the packaged systemd unit) — as the board's \
             uid it proves nothing"
        }
        Loopback => "unreachable for a real proxy request — report this",
    }
}
