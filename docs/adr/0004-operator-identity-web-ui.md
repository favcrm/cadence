# 0004 — Operator identity for the web UI: a minted session, never an ambient default

- Status: **accepted 2026-09-24 (operator)**, in PR #176 (CAD-313
  acceptance item 1, risk class `human`, trigger 1: auth or identity,
  `request_actor`, tailnet). Phase 1 is implemented under CAD-313 together
  with CAD-428; §12 records how. An independent security review is
  required before that implementation merges (acceptance item 3).
- Date: 2026-09-23
- Author: CAD-313 design lane (dispatched by `pm-opus-audit`)
- Deciders: the operator (every open question in §10), then an independent
  security reviewer (CAD-313 acceptance item 3) before implementation merges
- Issues: CAD-313 (this), CAD-309 (parent epic), CAD-280 (operator by
  positive proof), CAD-254 / #130 (board write caller), CAD-263 and CAD-276
  (accepted residuals), CAD-292 / #162 (`operator_proof` for force-release),
  CAD-310 (sandbox), CAD-312 (`setup` prints the login link), CAD-314
  (backup must exclude credentials), CAD-315 (macOS without `/proc`)
- Supersedes: the `src/ui.rs` module rule "No auth, by
  decision — containment is the defence", and the `docs/BOARD.md`
  write-identity row "walks cleanly and is tied to no pane → `operator (ui)`".
- **Code citations are pinned to `6507d4634636700433986991a5d2ad0d51587ec3`
  (origin/main, 2026-09-23).** Line numbers move. Find code by the quoted
  symbol, not by the line number.

## 1. Context

The board (`cadence ui`) is a synchronous HTTP server on `127.0.0.1:3010`.
Anyone who can reach it gets the operator's authority unless it can be
proved to be something else. CAD-254 narrowed this: a caller tied to a
registered pane now writes as that agent. CAD-280 names the flaw that
remains: **an unattributable local caller is treated as the operator.**
The operator's comment on CAD-280 records the root cause. Process-shape
heuristics cannot prove the operator under a single uid, because an agent
can `setsid -f` or start its own tmux server (independent review, PR #146).

CAD-309 (P0 Foundation) now wants a front door for public developers:
`install.sh`, then `cadence setup`, then a login link (CAD-312). The
onboarding plan also puts new operator-only endpoints behind the board:
launch, approve plans, answer permissions, merge, effect cards and
restore. Today the board cannot grant those to anyone safely, because it
cannot tell the operator from a detached agent.

### 1.1 What is measurably true right now

| Claim | Evidence |
|---|---|
| The board has no authentication, by design | `src/ui.rs:4`: "No auth, by decision — containment is the defence". |
| A caller tied to no pane writes as the operator | `write_caller` (`ui.rs:1248`): `Ok(None) => Ok(WriteCaller::Operator(request_actor(..)))`. `docs/BOARD.md` "Write identity" table, row 2. |
| A test pins that default | `tests/board.rs` `ui_write_caller_ignores_an_uncorroborated_env_alias` asserts `author == "operator"` for a plain local client. |
| The cross-site guards stop browsers, not processes | `write_guard` (`ui.rs:992`) checks content type, `X-Cadence-Board: 1`, and Origin / `Sec-Fetch-Site` **only when present**. `curl` sends neither header, and it can send any value it likes. |
| The daemon's positive proof exists, and has a known hole | `peer::operator_proof` (`peer.rs:233`). It is used by `slot_reconcile` and `approval_record/revoke` through `proven_operator` / `approval_operator` (`daemon.rs:2264`, `2345`), and by `rollout release --force` (`rollout.rs:828`, CAD-292). The residual is in its own doc comment: `setsid -f` plus a scrubbed env and stdio still passes. |
| An HTTP route that proxies an ungated daemon RPC is decorative | `POST /api/settings/model-defaults` calls `model_defaults_set` with `attribution: caller.actor()`. `rpc_model_defaults_set` (`daemon.rs:2487`) never checks the peer, so any same-uid process can call it on the socket. `rpc_monitor_alert_ack` (`daemon.rs:4021`) trusts a caller-supplied `by` the same way. |
| The daemon socket is same-uid, not authenticated | `daemon.rs:1-4`: a 0700 state dir, and `SO_PEERCRED` "establishes same-user access — it is not a[n authentication]". |
| This host's kernel settings | `kernel.yama.ptrace_scope = 1`: a process can ptrace only its own descendants, so an agent cannot read the UI server's or the daemon's memory. `kernel.apparmor_restrict_unprivileged_userns = 1`: unprivileged mount namespaces (`bwrap`, `unshare -Urm`) are blocked by default. Both were read from `/proc/sys` on 2026-09-23. |

### 1.2 Findings this design must answer

**F1 — Operator by absence.** Each detach shape (`setsid -f`, a
double fork, a private tmux server, `env -i`, stdio redirected to
`/dev/null`) turns a pane's child into a caller that is tied to no pane.
Today such a caller writes as `operator (ui)`. This is exactly the case
that CAD-313 acceptance item 2 forbids.

**F2 — A same-uid process can choose a tailnet name.** `request_actor`
(`ui.rs:1184`) trusts `Tailscale-User-Login` when three things hold:
tailnet mode is on, the TCP peer is loopback, and `Host` is the tailnet
name. A local process controls two of those three itself. It sets
`Host: <dns>:9450` and the header, connects to `127.0.0.1:3010`, and
detaches from its pane. `write_caller` then returns `Ok(None)` and the
write commits as `Actor: <any login> (tailscale)`. The test
`tailnet_write_is_attributed` sends this request shape from the test
process, which is same-uid and not the proxy, and it passes. Only the
direct-loopback `Host` case is covered as a forgery
(`forged_tailscale_headers_not_attributed`). So the forgery is not only
"operator by default". It lets the caller **name a specific person**.

**F3 — Gating HTTP alone gates nothing.** An operator-only board route
means something only if the daemon RPC behind it is operator-only too.
Otherwise an agent skips the board and calls the socket. Today
`model_defaults_set` is the live example.

**F4 — The operator's browser is usually on another machine.** This
host is headless. The operator reaches the board through the tailnet
proxy or an `ssh -L` tunnel. That browser, and any cookie in it, lives
on a laptop or phone that same-uid agents on this host cannot read.
This makes a cookie a much stronger credential here than it would be on
a laptop where the agents and the browser share a uid.

### 1.3 Threat model

The core constraint is this: **the agents run as the operator's uid.** The
kernel then sees the operator and an agent as the same principal for
files, `/proc`, signals, and the Unix socket. Nothing in-uid is a
boundary against a *determined* same-uid process. ADR 0001 §1.4 already
says this, and this ADR does not pretend otherwise. What this ADR can
change is **what an agent gets by default, and how overt it must be to
get more.**

| Adversary | Reach today | What this ADR must achieve |
|---|---|---|
| **Same-uid agent, ordinary behaviour** (curl from a pane, a `setsid` child, a scrubbed env, its own tmux server, a double fork) | Writes as `operator (ui)` (F1). Can forge a tailnet login (F2). Can call ungated daemon RPCs (F3). | **No operator authority by any process shape.** It is refused on operator-only routes and attributed to its pane (or refused) on agent-allowed routes. This is the target of acceptance item 2. |
| **Same-uid agent, deliberate credential theft** (it reads the operator secret file, *and* evades `operator_proof` via `setsid -f` with a scrubbed env and stdio) | Not applicable today (there is nothing to steal, because the default already grants operator). | **Phase 1: overt, multi-step and detected (§5.10), not prevented.** **Phase 2 (§5.11): prevented** by making the file unreadable from agent process trees. |
| **Same-uid agent that reads the operator's browser cookie store** | Not applicable | Possible only when the browser runs on this host as this uid. It is not possible for a tailnet or ssh-tunnel browser (F4). This is stated as a residual. |
| **Agent-authored content rendered in the operator's browser** (issue bodies, comments, artifacts in the SPA) | XSS would reach nothing that a curl could not already reach | **This becomes the most important path once a cookie exists.** Script that runs on the board origin can *ride* the session, even though it cannot read an HttpOnly cookie. The CSP (`ui.rs` `CSP`, no inline script), the markdown sanitiser, and the forced download of html/svg artifacts move from defence in depth to load-bearing. The security review must re-audit them. |
| **Cross-site page in the operator's browser** (CSRF) | Blocked by the four guards | Unchanged guards. In addition, `Origin` becomes **mandatory** on any request that carries the session cookie, and the cookie is `SameSite=Strict`. |
| **DNS rebinding** | The `Host` allowlist returns `421` | Unchanged. The cookie is host-only, so a rebinding origin never carries it. |
| **Another local uid** (a service account, another human on a shared host) | Its socket owner is invisible, so `tcp_peer_pane` returns `Err`, and the write is refused unless it is tailnet-shaped (then F2 applies) | The 0600 secret and the 0700 directory exclude it. It cannot mint. Its socket uid is not tailscaled's, so tailnet headers are ignored. |
| **Tailnet peer** (another device or another member admitted by Tailscale ACLs) | Reads everything. Writes as its own login. | Writes need a session. Its login names the actor only through the verified proxy (§5.8). Whether tailnet **reads** need a session is open question Q4. |
| **Stolen login link** | Not applicable | Single use, 120 s TTL, bound to one origin, carried in the URL fragment (never sent to the server, never logged). A used or expired link is refused loudly, and the operator sees "already used". |
| **Stolen session cookie** | Not applicable | Host-only, HttpOnly, and expiring. Revocable per session or all at once. A pane-tied peer that presents it is refused and the session is revoked (§5.6). |
| root, kernel, or physical access | Everything | Out of scope. |

## 2. What "done" looks like

- **D1 No ambient operator.** No request is ever granted operator
  authority because of what it *lacks* (a pane tie, a TTY, an env
  alias). Operator authority is granted only on positive possession of a
  valid session, or, on the CLI, a proof plus possession of the secret.
- **D2 Agents are refused, whatever their process shape.** A
  same-uid agent process, in every detach shape named in F1, gets `403`
  on every operator-only route. This is acceptance item 2, tested per
  route and per shape (§7).
- **D3 The operator still works.** A browser on loopback, over an ssh
  tunnel, or over the tailnet can log in once per device with a link
  from the CLI, and then can use every route.
- **D4 One authority.** The daemon mints and checks sessions. The board
  asks the daemon. Operator-only daemon RPCs accept the same two proofs
  whether the request arrives through the board or through the CLI (F3).
- **D5 Honest residuals.** Every path that still yields operator without
  the operator is written down here, is pinned by a test where one is
  possible, and is re-stated in `docs/BOARD.md` and `docs/AUDIT.md` at
  implementation.
- **D6 Break-glass.** Losing the browser, the secret or the daemon never
  locks the operator out. Every operator action has a CLI path.

## 3. Options

### Option A — Do nothing

This keeps F1 to F3. It fails acceptance item 2 by construction. CAD-309
cannot add operator-only endpoints (launch, merge, effects, restore)
without handing them to every agent. Rejected.

### Option B — Smallest thing: refuse unattributable writes, add no login

Flip `Ok(None)` in `write_caller` to a refusal. Agents are then refused
or attributed. But the operator's own browser is *also* unattributable,
because it is a remote process that holds no terminal. It would lose all
writes and have no way back in. B is correct about agents but locks out
the operator. It is kept as a component of C, and is not a decision on
its own.

### Option C — Proposed: a 0600 secret, a single-use link, and an HttpOnly session, with the daemon as the authority

This is the design in §5. It is B plus a way for the operator to prove
itself that a same-uid agent does not have by default. Phase 2 (§5.11)
then makes that proof something an agent *cannot* get.

### Option D — Rejected as the default: OS-user separation for agents

Run agents as a separate uid, such as `cadence-agent`. This is the only
option that creates a real kernel boundary: a 0600 file, `/proc`
environ and fds, and ptrace would all separate the operator from the
agents for free. It is rejected as the *default* for these reasons:

- **Install needs root.** Creating users needs root. CAD-311's
  `install.sh --prefix` is an unprivileged, per-user install for public
  developers on Linux and macOS.
- **Provider credentials live in the operator's home.** Claude, Codex,
  Cursor and Devin logins, `gh` auth, ssh keys and git identity are all
  there. Agents need them. Sharing them across uids means either copying
  credentials into a second home, which multiplies secrets (see CAD-108),
  or group-readable permissions, which undoes the separation.
- **Shared files break.** Worktrees are edited by both the agents and
  the operator. Separate uids mean group ownership, umask and ACL
  management in every repository, and `git` refuses repos owned by
  another user (`safe.directory`).
- **tmux breaks.** The tmux socket and the pty adapter assume one user.
  Driving a pane across uids needs socket permissions that re-open the
  hole.

It is kept as a **supported hardened deployment**: nothing in C assumes
a single uid. With agents under another uid, the 0600 file in §5.1
becomes a real boundary without any code change. It is also one of the
Phase 2 candidates (§5.11).

### Option E — Rejected: tailnet identity only

Treat a verified `Tailscale-User-Login` as the operator, with no login.
Reasons:

- **Not everyone has Tailscale.** CAD-309's audience is public
  developers, many with a browser on the same laptop and no tailnet.
  Loopback still needs an answer.
- **A tailnet login is not the operator.** Tailnets have several
  members and shared nodes, so identity would need a login allowlist.
  That allowlist is a second operator registry.
- **The header can be forged today** from the same uid (F2).
- **Hairpin traffic may carry the operator's name.** A request from this
  host to its own tailnet name goes through tailscaled. It is expected to
  carry the identity of *this node's owner*, who is often the operator.
  If that holds, a same-uid agent can obtain a genuine proxy connection
  bearing the operator's login. The implementation must test this
  (§7, T-TS3). Until it is shown false, it is assumed true.

Tailnet identity is kept for **attribution** (the name of the actor), and
only through the socket-uid check in §5.8. It is never used for
**authority**. Q5 asks whether to allow a login allowlist as a
convenience on top.

### Option F — Rejected: a TTY or pane-ancestry proof

This is today's mechanism, strengthened: `operator_proof`,
`derived_caller`'s foreign-TTY rule, and the session-leader check. It is
rejected as *the* proof for four reasons:

- **Proven defeatable.** PR #146 and the residual in `operator_proof`'s
  doc comment show that `setsid -f` plus a scrubbed env and stdio passes.
  Each new heuristic invites a new detach shape. This is the arms race
  that CAD-280 asks to end.
- **It proves nothing for HTTP.** The operator's browser is a remote
  process behind tailscaled or sshd, and holds no terminal (F4). Both the
  operator and a detached agent are "tied to nothing".
- **Terminal output is not private.** If the operator's shell runs inside
  the user's tmux server, a same-uid agent can `capture-pane` it.
- **There is no `/proc` on macOS** (CAD-315).

It is kept as a **narrowing layer**. Minting a link requires
`operator_proof` (§5.2), so the ordinary agent shapes fail at the mint
before the secret is even compared.

### Option G — Rejected for phase 1: a hardware key or WebAuthn

WebAuthn with user verification (a touch or biometric) is the only option
that proves a *human is present*. A same-uid agent cannot press the key.
It is rejected for now, not forever:

- **Bootstrap.** Registering the first authenticator already needs a
  proven operator session. So C (or something like it) is needed anyway,
  and WebAuthn is a step on top of it, not a replacement.
- **Origin rules.** A relying-party ID must be a domain. `127.0.0.1` is
  not allowed. `localhost` / `cadence.localhost` and the tailnet name
  work. Every origin the operator uses must be registered.
- **Supply chain.** CBOR and COSE verification means new crates. That is
  risk-class trigger 4 on top of trigger 1.
- **Recovery.** Losing the key falls back to the link flow. The overall
  strength is therefore the link flow's, unless recovery is made harder.

It is the natural **step-up** for the highest-risk future actions
(approving a merge of class `human`, pressing effect cards, restore).
Proposed as a follow-up after C, not part of CAD-313.

### Option H — Rejected: a long-lived token in the URL (Jupyter `?token=`)

A query-string bearer token leaks. It lands in browser history, proxy and
server logs, shell history, screenshots and `Referer` (mitigated here by
`no-referrer`, but not elsewhere). It is also reusable. C uses a
**single-use** nonce in the **fragment** instead. The fragment is never
sent to any server.

### Option I — Rejected: a token in `localStorage` sent as `Authorization`

The SPA renders agent-authored markdown. Any XSS could read
`localStorage` and take the credential off-host. An HttpOnly cookie can
be *ridden* by XSS, but it cannot be *exfiltrated*, and it dies with
revocation. The CSRF guards already exist for cookie-borne authority.

### Option J — Rejected: sessions held only in the UI server process

This was simpler, but it makes two authorities. The daemon RPCs behind
operator routes (F3) could not verify the session, and the CLI and the
board would not share one rule. It is also lost when the board
restarts, and `ui tailscale start` restarts the board. The daemon owns
sessions (D4).

## 4. Decision

Adopt **Option C in two phases**.

- **Phase 1 (CAD-313):** operator authority on the board requires a
  daemon-issued session. A session is obtained only by exchanging a
  single-use link. A link is minted only by the CLI from a process that
  passes `operator_proof` **and** presents the 0600 operator secret. The
  board never infers the operator from absence (F1). Tailnet headers name
  the actor only when the proxy socket's owner is tailscaled (F2).
  Operator-only daemon RPCs require the same authority (F3). Every route
  is classified in one table, and **an unclassified write route is
  operator-only** (fail closed).
- **Phase 2 (a new ticket, aligned with CAD-280):** make the secret
  unreadable from agent process trees. This turns the phase-1 residual
  (steal the secret *and* evade the proof) into a refusal. Candidate
  mechanisms and their limits are in §5.11. Choosing among them is not
  part of this ADR.

**The trade-off with same-uid agents, stated once.** The 0600 file does
not stop an agent. It is **the thing a boundary can later be drawn
around**, and in the meantime it forces impersonation to be a
deliberate, multi-step and detectable act rather than the default. Phase
1 ends *ambient* operator authority. Phase 2 ends *stealable* operator
authority. Only OS-user separation (D) or a human-presence factor (G)
would end it against an agent that escapes its confinement.

## 5. Design

### 5.1 The operator secret — what the 0600 file does and does not protect

- **Location:** `<state dir>/operator/secret`. The directory is `0700`,
  the file is `0600`, and both are owned by the daemon's euid. The
  content is 32 bytes from `getrandom`, base64url-encoded.
- **Creation:** by `cadence setup` (CAD-312), or by the daemon at start
  if it is missing, so that upgrades need no manual step. It is created
  with `O_CREAT|O_EXCL|O_NOFOLLOW`, mode `0600`.
- **Strict modes, in the manner of ssh:** every reader (the daemon and
  the CLI) opens it with `O_NOFOLLOW` and **refuses** in four cases: the
  file is not a regular file, the owner is not the euid, the mode has any
  group or other bit, or the directory is not `0700` and owned by the
  euid. The refusal names the fix (`chmod 600 …`). It never "repairs"
  silently.
- **It never enters the environment or argv.** It is never in an env
  var, an argument, a URL, a log line, a tracker commit, or a briefing
  (CAD-108). The CLI reads it from the file and sends it only over the
  0700 Unix socket.
- **Backups must exclude it.** CAD-314's backup and export must exclude
  `operator/`, which that ticket's acceptance already demands for
  credentials. A restore on a new host generates a new secret, and
  sessions do not transfer.

| The 0600 file **does** protect against | The 0600 file does **not** protect against |
|---|---|
| Other local uids: service accounts, other humans on a shared host | **Any process with the operator's uid**: agents, their detached children, and anything they start through `systemd --user`, cron, `at`, or the operator's tmux server |
| Browsers, tailnet peers and remote hosts, which never see it; only the fragment nonce derived from it travels | root |
| Accidental disclosure through git, the tracker, briefings, or exports (it lives outside all of them and is excluded from backups) | A backup or disk image that someone includes it in by hand |
| **In phase 2: agent process trees**, once they are confined from `operator/` (§5.11) | A confined agent that escapes through a deputy running outside the confinement (§5.11) |

The file is *not* what distinguishes an agent from the operator in phase
1. §5.6 does that. The file is an additional factor for the mint, and
the hook that phase 2 needs.

### 5.2 Minting a single-use link (CLI → daemon)

```
cadence ui login [--tailnet] [--qr] [--open]
cadence ui login --rotate        # rotate secret, revoke everything, then mint
```

1. The CLI reads the secret (§5.1 strict modes) and connects to the
   daemon socket.
2. It calls the new RPC `operator_link_mint {secret, origin}`. The origin
   is `loopback` or `tailnet`.
3. The daemon checks, in order, and fails closed:
   - The peer derives no slot identity (`slot_identity`, as in
     `slot_reconcile`).
   - The peer passes `proven_operator` (`peer::operator_proof`).
   - Identity-shaped params (`by`, `actor`, `alias`, …) are refused, as
     in `approval_operator`.
   - The secret matches the in-memory copy, compared in constant time.
4. The daemon returns a **nonce**. It is 32 random bytes, valid for
   **120 s**, **single-use**, and bound to that origin. Only its SHA-256
   is kept in memory. The daemon records the event `operator_link_minted`
   with the peer pid and the origin, but never the nonce.
5. The CLI prints the link, with the nonce **only in the fragment**:
   - loopback: `http://cadence.localhost:<port>/login#n=<nonce>`
   - tailnet: `https://<dns>:<https port>/login#n=<nonce>`

   `--qr` renders it for a phone (`qr_term` already exists). `--open`
   launches the browser when a desktop is present.

Why `cadence.localhost` and not `127.0.0.1`: **cookies ignore ports.** A
cookie set by `127.0.0.1:3010` is sent to every server on `127.0.0.1`,
including an agent's dev server on `127.0.0.1:5173`, whose logs would
capture it. `*.localhost` resolves to loopback in current Chrome and
Firefox (RFC 6761), and gives the cookie a host no agent tooling uses.
The `Host` and `Origin` allowlists gain `cadence.localhost:<port>`.
Today they allow `cadence.localhost` only bare and at `:18000`
(`host_allowed`, `origin_allowed`). The same reasoning applies on the
operator's laptop through `ssh -L`.

`cadence setup` (CAD-312) and `cadence ui tailscale start` end by
running this mint, so first use needs no extra step.

### 5.3 Exchanging the link for a session (browser → board → daemon)

1. The browser opens `/login#n=…`. The static handler already falls back
   to `index.html`, so the SPA's login view loads. **The fragment never
   reaches the server**, so it never appears in `ui.log`.
2. The SPA reads `location.hash`, immediately calls
   `history.replaceState` to strip it, and sends
   `POST /api/session {nonce}` with the normal write guards (JSON,
   `X-Cadence-Board: 1`, a same-origin `Origin`). Prefetchers and
   link-preview bots do not run the script, so they cannot consume the
   link.
3. The board forwards the nonce to `operator_session_open {nonce, origin,
   host}`. The daemon checks the hash, the TTL, single use, and that the
   origin matches the `Host` the request arrived on. It then deletes the
   nonce and creates a session.
4. The daemon returns the session token. The board sets the cookie
   (§5.4), responds `204` with `Cache-Control: no-store`, and the SPA
   reloads `/api/meta`, which now reports `operator: true`.
5. A nonce that is already used, expired, or for the wrong origin
   returns `403 {check: "login_link"}` with the reason. The daemon emits
   `operator_link_rejected`. An *already used* rejection is raised as an
   alert, because it means someone else exchanged the link first.

### 5.4 The session cookie

```
Set-Cookie: cadence_operator=<token>; Path=/; HttpOnly; SameSite=Strict; Max-Age=<absolute remaining>
Set-Cookie: __Host-cadence_operator=<token>; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=…   # tailnet https origin
```

- The token is 32 random bytes. The daemon stores only
  `sha256(token)`, plus: created, last used, absolute expiry, origin,
  display id (the first 8 hex digits of the hash), and the user agent
  at exchange.
- **Host-only:** no `Domain` attribute. `__Host-` on https enforces
  `Secure`, `Path=/` and no `Domain`.
- **HttpOnly:** SPA script never reads it (Option I).
- **SameSite=Strict**, plus the existing guards, plus **`Origin` becomes
  mandatory** whenever the cookie is present. Today `write_guard` checks
  `Origin` only "when the browser sends them". Browsers always send it on
  `POST`, `PATCH` and `DELETE`, so this costs a real browser nothing.
- **Persistence:** `<state dir>/operator/sessions.json` (0600, tmp plus
  rename) holds **hashes only**. A daemon or board restart keeps
  sessions. Reading the file yields nothing usable. There is no store
  migration.

### 5.5 Expiry, rotation, revocation

| Item | Proposed default | Notes |
|---|---|---|
| Link nonce TTL | 120 s, single use | Long enough to paste or scan a QR code |
| Session idle expiry | 24 h | Refreshed at most once a minute by a verified request |
| Session absolute expiry | 7 days | The operator then runs `ui login` again. No sliding past this. |
| Session token rotation | New token at each login. No per-request rotation. | Deliberately simple. Long-lived SSE tabs and several tabs do not race. |
| Secret rotation | `cadence ui login --rotate` or `cadence ui sessions --revoke-all --rotate` | Replaces the file, revokes every session and nonce, then mints |
| Revocation | `POST /api/session/logout` (this session). `cadence ui sessions [--revoke <id> \| --revoke-all]` (operator-proof gated). | `ui sessions` lists display id, origin, created, last used and user agent |

Q3 asks the operator to confirm these defaults.

### 5.6 How each caller is decided — the one rule for the board

Every request that is not public-read goes through one function.
`write_caller` is replaced by `board_caller`. It combines the session
(possession) with the existing `tcp_peer_pane` (attribution):

| Session cookie | Peer attribution (`tcp_peer_pane`) | Result |
|---|---|---|
| valid | `Ok(Some(pane))`: the peer is tied to a pane | **Refuse** `403 {check: "session_from_agent"}`. **Revoke the session.** Raise an alert. An operator cookie presented by an agent process is evidence of theft. |
| valid | `Ok(None)` or `Err` (browser via sshd, tailscaled, or no `/proc`) | **Operator.** Actor `operator (ui)`, or `<login> (tailscale)` per §5.8 |
| absent or invalid | `Ok(Some(pane))` | **Agent(alias)**, only on agent-allowed routes. Operator-only routes return `403 {check: "operator_session_required"}`. |
| absent or invalid | `Ok(None)` | **Refuse** `403 {check: "operator_session_required"}`. **This is the F1 flip.** |
| absent or invalid | `Err` | **Refuse** `403 {check: "caller_identity"}`, as today |

**How agents are told apart, even from loopback and even under the same
uid:** the rule does not need to recognise an agent as an agent. An agent
never *holds* a session: it cannot mint one without passing
`operator_proof` and reading the secret. The board never grants operator
without a session. Pane attribution still positively identifies agents
for agent-allowed routes and for the theft check. It is no longer the
only thing between an agent and operator authority.

`GET /api/meta` gains `operator: bool`, `session: {display_id,
idle_expires_at, expires_at} | null`, and `login_hint` (the CLI command
to run). It keeps `actor`, `read_only` and `tailnet_url`. The SPA hides
controls for operator-only routes when `operator` is false.

### 5.7 How the CLI proves operator identity

The CLI does not use the cookie. It talks to the daemon over the socket.

- **Operator-only CLI verbs** (`slot reconcile`, `approval
  record/revoke`, `rollout release --force`, `ui login`, `ui sessions`,
  and now `model_defaults_set` under F3) require `operator_proof` *and*
  that the calling process can read the secret (§5.1). For daemon
  verbs, the secret is presented in the RPC. `rollout release --force`
  works with the daemon stopped (CAD-292), so it proves possession by
  reading the file itself.
- In phase 1, "can read the secret" adds nothing against an unconfined
  same-uid agent. It adds a portable factor on macOS, where
  `operator_proof` degrades (CAD-315). **In phase 2 it becomes the
  factor that closes CAD-280's `setsid -f` residual for the CLI too**,
  because confinement applies across `setsid` and `fork`.
- Whether phase 1 adds the secret check to the *existing* verbs
  (`slot_reconcile`, approvals, rollout), or only to the new ones, is
  open question Q7.

**The rule, stated once for both front doors:** *operator authority is
possession, checked by the daemon — a session token (web) or the secret
(CLI) — gated by positive process proof wherever the process is local.
Absence of evidence is never authority.*

### 5.8 Tailnet

- **Authority:** the same as loopback. A session is required, and it is
  minted with `ui login --tailnet` and exchanged on the tailnet origin.
- **Attribution (F2 fix):** `Tailscale-User-Login` is considered only
  when the connection's client socket belongs to **tailscaled's uid**
  and not to ours. The uid comes from the `uid` column of the same
  `/proc/net/tcp{,6}` line that `client_socket_inode` already parses. A
  same-uid process cannot create a socket owned by root. Otherwise the
  header is ignored and the actor is `operator (ui)`.
- **Configuration:** the tailscaled uid is read once from `tailscale`
  when sharing is armed (normally `0`) and stored in `ui.json`. If it
  cannot be read, header trust is off. That fails closed to `operator
  (ui)`.
- **macOS** (the App Store Tailscale runs as the user): header trust is
  off. This is recorded for CAD-315.

### 5.9 Endpoint classification

These are the classes. **public-read** is any caller that passes the
`Host` allowlist; no session. **agent-allowed** means an operator
session, *or* a caller attributed to exactly one pane (the write carries
the pane's alias and is never `operator`). **operator-only** means an
operator session (web) or the CLI proof (§5.7).

Every current route in `src/ui.rs` (`handle`, `write_route`):

| Route | Method | Class | Why |
|---|---|---|---|
| `/`, `/assets/*`, `/login` and the SPA fallback | GET/HEAD | public-read | The static bundle. `/login` handles the fragment client-side. |
| `/api/health` | GET | public-read | Used by `ui status` / `http_get` probes with no cookie |
| `/api/meta` | GET | public-read | The SPA needs it before login. It reports the session state. |
| `/api/overview` | GET | public-read | Agents can already read the same state from the state dir and `~/pm` |
| `/api/projects`, `/api/projects/:key/context` | GET | public-read | As above |
| `/api/issues`, `/api/issues/:id`, `/:id/file`, `/:id/activity`, `/:id/history` | GET | public-read | As above |
| `/api/issues/:id/artifacts/:name` | GET | public-read | As above. The existing CSP sandbox and forced download stay. |
| `/api/epics` | GET | public-read | As above |
| `/api/agents`, `/api/agents/:alias` | GET | public-read | As above (see Q4 for the tailnet) |
| `/api/memories`, `/api/memories/:project/:slug` | GET | public-read | As above |
| `/api/settings/model-defaults` | GET | public-read | A read snapshot |
| `/api/stream` | GET | public-read | SSE invalidation only, no data |
| `POST /api/issues` | POST | agent-allowed | The same `issue::write` path agents already reach through the `cadence issue` CLI. The board must be neither a weaker nor a stronger door than the CLI. |
| `PATCH /api/issues/:id` | PATCH | agent-allowed | As above. *Which statuses a role may set* is ADR 0001's capability model, not identity. |
| `POST`/`DELETE /api/issues/:id/links` | POST/DELETE | agent-allowed | As above |
| `POST /api/issues/:id/refs` | POST | agent-allowed | As above |
| `POST /api/issues/:id/comments` | POST | agent-allowed | As above. The author is the derived caller. |
| `POST /api/issues/:id/artifacts` | POST | agent-allowed | As above |
| `POST /api/monitors/:m/alerts/:seq/ack` | POST | agent-allowed | PM agents acknowledge alerts today through `monitor_alert_ack`. Recorded `by` is derived, never read from the body. See Q6. |
| `POST /api/settings/model-defaults` | POST | **operator-only** | Fleet-wide model and effort defaults. **Requires the daemon gate (F3).** |
| `POST /api/memories/:project/:slug/accept\|reject` | POST | **refused** (unchanged) | Memory authority is a native PM endpoint, not the operator. HTTP cannot prove it. |
| *new* `POST /api/session` | POST | public (the nonce is the credential) | Write guards apply. Rate-limited to 10 failures per minute. |
| *new* `POST /api/session/logout` | POST | session holder | Revokes only the presenting session |

**Rule for future routes.** The classification lives in **one table in
code**: a route pattern, the method, and a class. Dispatch and the
criterion-2 test both read it. A write route with no entry is
**operator-only**. The planned operator surfaces (launch, approve plan,
answer permission, merge, effect cards, connect platform, restore) are
operator-only, and each must have a CLI equivalent (D6).

**Daemon counterpart (F3).** The board refusing is not enough. Each
operator-only board route's RPC must itself require operator authority:
either `proven_operator` plus the secret (CLI), or `operator_session`
verified by the daemon (forwarded by the board). In phase 1 this means
`model_defaults_set`. `monitor_alert_ack` stays agent-allowed, but its
`by` is derived from the peer instead of taken from params.

### 5.10 Detection (phase 1, best effort)

This follows charter principle 6 (detect, then automate).

- The daemon holds an inotify `IN_OPEN` watch on `operator/secret`.
  inotify needs no privilege for a file this uid owns. An open that does
  not fall within a few seconds of an `operator_link_mint`, a rotation,
  or a CLI operator verb raises the monitor alert
  `operator_secret_read` on the Overview "needs me" rail.
- inotify reports **no pid**, so this is a signal and not an
  attribution. It is racy by design. It **does not auto-rotate**: an
  agent that reads the file in a loop must not be able to lock the
  operator out (a DoS). The alert recommends `ui login --rotate`.
- The alerts `session_from_agent` (§5.6) and `operator_link_rejected`
  for an already-used link (§5.3) are the other two tripwires.

### 5.11 Phase 2 (not part of CAD-313): make the secret unreadable to agents

The goal: no process in an agent's tree, including its `setsid`, double
fork and own-tmux descendants, can open `operator/`. With that in place,
both the web mint and the CLI operator verbs refuse agents on possession
alone. `operator_proof` becomes a second layer instead of the only one.
This closes CAD-280 acceptance item 2 for every detach shape, because
the confinement is inherited, not inferred.

Candidates and known limits, for the phase-2 ticket to choose from:

| Mechanism | Strength | Limits |
|---|---|---|
| OS-user separation (Option D) | A real kernel boundary | The install, credential and worktree costs in §3 D |
| Landlock, applied by cadence when it launches a pane command | Unprivileged, inherited across `setsid` and `fork`, and cannot be dropped | It is allow-list semantics: it cannot express "all of `/` except one directory". The rule set must enumerate siblings, and new paths created later escape it. The ABI depends on the kernel. There is no macOS equivalent. |
| A mount namespace (`bwrap`, `unshare -Urm`) that hides `operator/` | Simple to express | Blocked on this host by `apparmor_restrict_unprivileged_userns=1` unless an AppArmor profile is installed, and installing one needs root |
| Provider sandboxes (e.g. Claude Code or Codex filesystem deny-read settings) | Uses what providers already ship | Per provider and per version. Not all providers have one (Devin, Cursor). The settings file must itself be out of the agent's reach. |

**Confused deputies** that every mechanism must also close, because a
confined agent can ask an *unconfined* same-uid process to act for it:
`systemd-run --user`, cron and `at`, the operator's tmux server socket
(`tmux -S … send-keys`, `new-window`), and the cadence daemon itself
(any RPC that runs a command, such as a dispatch or a hook). Phase 2 is
not done until each is either closed or listed as a residual.

## 6. Migration and rollout

**Existing installs (this host and early users):**

1. **Ship phase 1 in `observe` mode.** Add `ui.json` `auth: "observe" |
   "enforce"`, defaulting to `observe` for one release. Sessions, login,
   `/api/meta.operator` and every refusal path are live, *except* one:
   an unattributed request with no session is still allowed as `operator
   (ui)`. It is logged as `would_refuse` (`ui.log` plus the daemon event
   `ui_auth_would_refuse` with the route and peer). This finds operator
   scripts that `curl` the board. Q6 asks whether to skip `observe`.
   - F2 (tailnet header trust through the socket uid) and F3 (the daemon
     gate on `model_defaults_set`) ship **enforced** from day one. They
     are not needed for the operator's own browser to work.
2. **First start after the upgrade:** the daemon creates the secret. `ui
   start` prints one line: `board writes will need a login: run cadence
   ui login`. `ui status` shows the auth mode, the session count and
   secret health (the strict-modes result).
3. **Enforce:** the next release defaults to `enforce`. The operator can
   flip early with `cadence ui start --auth enforce`. `observe` then
   stays available for one more release and is then removed.
4. **Tests that pin old behaviour flip deliberately, not silently:**
   - `ui_write_caller_ignores_an_uncorroborated_env_alias` (author
     `operator` → `403 operator_session_required`)
   - `tailnet_write_is_attributed` (must now come from a tailscaled-uid
     socket, or it is `operator (ui)` with a session, or refused without
     one)
   - `ui_write_caller_pty_tie_is_forgeable_residual_pinned` (unchanged
     for agent-allowed routes; see §8)
5. **Docs updated with the code, not before:**
   - `docs/BOARD.md`: remote access, the write-identity table, and the
     threat-model four lines
   - the `docs/AUDIT.md` residuals
   - the `src/ui.rs` and `src/peer.rs` module docs

**New installs (CAD-311 and CAD-312):** `setup` creates the secret,
starts the board in `enforce`, and ends by printing the link and a QR
code.

**Sandbox (CAD-310):** a sandbox has its own state dir, and so its own
secret and sessions. Whether agents that QA a sandbox board may mint
sandbox logins is Q8.

**Break-glass (D6):**

| Situation | Path |
|---|---|
| Browser lost, cookie cleared, session expired | Run `cadence ui login` again from an operator shell |
| Link or cookie suspected stolen | `cadence ui sessions --revoke-all --rotate` |
| Secret file damaged or has wrong mode | The strict-modes refusal names the fix. `cadence ui login --rotate` recreates it (operator-proof gated). |
| Daemon down | Board operator writes refuse (the daemon is the authority), and reads still work. Every operator action has a CLI path. `rollout release --force` already works with the daemon stopped (CAD-292). |
| The auth code itself is broken | `cadence ui start --read-only` or `ui stop`. Operate through the CLI. There is deliberately **no** "legacy loopback operator" switch, because it would be the hole under another name (see Q9). |
| `operator_proof` refuses the real operator (for example an unusual login shell shape) | The refusal names the failing check (the existing behaviour). Run from a plain ssh shell. This is a bug to file, not something to bypass. |

## 7. Test plan

The integration tests extend `tests/board.rs`. They use its `UiDaemon`,
`plant_pane` and `/dev/tcp` client helpers, a temporary state dir, and a
random port. **They never touch the production board on `:3010`.** Where
a real browser is needed, it runs against a CAD-310 sandbox board.

### Criterion 2 — the agent matrix (T-A)

Iterate **the route table from §5.9** (not a hand-written list), so that
a new route cannot skip the test. First assert that the table has a
non-zero number of operator-only routes, so an empty table fails
instead of passing (CAD-138). Then for each operator-only route, and
each caller shape below, assert `403` with the named `check`, and assert
that **nothing was written** (the tracker HEAD and the daemon store are
unchanged):

| ID | Caller shape | Expected |
|---|---|---|
| A1 | A descendant of a planted pane (curl child) | `operator_session_required` |
| A2 | `setsid -f` child of a pane, `env -i`, stdio `</dev/null >/dev/null 2>&1` | `operator_session_required` |
| A3 | Double-fork orphan (the intermediate exits) | `operator_session_required` |
| A4 | Its own tmux server (`tmux -L t new -d …`) | `operator_session_required` |
| A5 | A process tied to no pane, all four guard headers forged, `Origin` allowlisted, `Sec-Fetch-Site: same-origin` | `operator_session_required` |
| A6 | A5 plus `CADENCE_ALIAS=<registered>` | `operator_session_required` |
| A7 | A5 plus tailnet `Host` and a forged `Tailscale-User-Login` | `operator_session_required`. On an agent-allowed route with a session: actor `operator (ui)`, **never** the forged login (F2). |
| A8 | Pane's pty opened onto stdio (the CAD-276 lateral shape) | `operator_session_required` |
| A9 | A valid operator cookie replayed from a pane-descendant process | `session_from_agent`, **and the session is revoked**: a follow-up request from a browser-shaped peer with the same cookie also fails |
| A10 | `operator_link_mint` from each of A1 to A4 | refused by `slot_identity` / `proven_operator` |
| A11 | `operator_link_mint` from a non-pane process with a wrong or absent secret | refused (`secret`) |
| A12 | Daemon RPC `model_defaults_set` over the socket from A1 and A2 | refused (the F3 gate) |

**The residual is pinned, not hidden.** One test,
`operator_secret_theft_residual_pinned`, reproduces the phase-1
residual: a `setsid -f` process with a scrubbed env and stdio that reads
the secret, mints and exchanges. It asserts that the mint **succeeds**
and that `operator_secret_read` fires. It carries the same kind of
comment as `ui_write_caller_pty_tie_is_forgeable_residual_pinned`:
phase 2 must flip it deliberately.

### The operator path works (T-O)

- O1: A non-pane process mints (it passes `operator_proof` and holds the
  secret), exchanges on `cadence.localhost:<port>`, then writes to an
  operator-only route and to an agent-allowed route. Both return `200`,
  and the `Actor:` trailer is `operator (ui)`.
- O2: `/api/meta` before and after login (`operator` false → true), and
  after logout (false).
- O3: The session survives a board restart and a daemon restart
  (`sessions.json` holds hashes only; assert that the raw token does not
  appear in the file).
- O4: An agent-allowed write from a pane-descendant peer with no session
  is still attributed to the pane (the CAD-254 behaviour is preserved).

### The link and the cookie (T-L)

- L1: The nonce is single-use (a second exchange returns `403
  login_link` and raises the alert).
- L2: The nonce expires (inject a clock: 121 s → refused).
- L3: A nonce minted for `tailnet` is refused on a loopback `Host`, and
  the reverse.
- L4: `Set-Cookie` has `HttpOnly`, `SameSite=Strict`, `Path=/`, no
  `Domain`, and `Max-Age`. It uses `__Host-` and `Secure` on the https
  origin.
- L5: A request with the cookie and no `Origin` is refused. A request
  with the cookie and a foreign `Origin` is refused.
- L6: Idle and absolute expiry (clock injection). `--revoke`,
  `--revoke-all` and `--rotate` each invalidate the right set.
- L7: `ui.log` and every daemon event contain neither the nonce nor the
  token. Grep the whole log for both values, after first asserting that
  the log is non-empty.

### The secret file (T-S)

- S1: Created with mode `0600` in a `0700` directory.
- S2: Refused, and never repaired, in each case: mode `0640`, `0604`, a
  symlink, a foreign owner (where testable), and a directory with mode
  `0755`.
- S3: Not in `ui.json`, env, argv (`/proc/<cli>/cmdline`), or a CAD-314
  export bundle.

### Tailnet (T-TS)

- TS1: The header is honoured only when the client socket's uid is the
  configured tailscaled uid. The test injects the uid table, because
  tests cannot create a root socket.
- TS2: A same-uid socket with a tailnet `Host` and the header gets
  `operator (ui)` (with a session) or a refusal (without one).
- TS3: **Manual, on a real tailnet, before phase 1 is enforced:** a
  request from this host to its own tailnet URL. Record whether
  tailscaled attaches the node owner's login. This settles Option E's
  hairpin question. Record the result in the security review.

### Browser (T-B, agent-browser against a sandbox board)

- B1: Log in through the link, and the fragment is gone from the address
  bar after the load.
- B2: A page served from `http://127.0.0.1:<other port>` cannot write
  (the preflight fails, and `Sec-Fetch-Site` is `same-site`, not
  `same-origin`).
- B3: A DNS-rebinding `Host` gets `421`.
- B4: Agent-authored markdown with script payloads (in an issue body, a
  comment, and an artifact) does not execute under the CSP. This is the
  load-bearing XSS check from §1.3.

### Independent security review (acceptance item 3)

The reviewer is not the author and not the implementer. They are given
this ADR, the diff and the test run. They must answer, in the review
record:

- Is every path to operator authority in §5.6 and §5.7? Is anything
  missing?
- Is the residual in §1.3 stated accurately?
- Is the TS3 hairpin result recorded?
- Are the markdown sanitiser and the CSP adequate now that a session
  exists?
- Constant-time comparison, randomness source, and log hygiene.
- Is the §5.9 classification right for every route?

## 8. Consistency with related decisions — conflicts named

**CAD-280 (operator by positive proof).**
- *Aligned:* CAD-280's acceptance item 1 suggests exactly this ("a board
  UI session token minted by an operator-only CLI step"). Its "what an
  unproven local caller gets" is answered here: reads are public, writes
  are refused (§5.6).
- *Conflict:* CAD-280's item 2 wants *no* detach shape to obtain
  operator attribution, **for the CLI verbs too**. Phase 1 does not meet
  that for a process that reads the secret and uses the `setsid -f`
  residual. **Phase 2 does.** Proposal: this ADR serves as CAD-280's
  design record. CAD-280 stays open, owns phase 2, and owns re-evaluating
  CAD-263 and CAD-276 (its item 3).

**CAD-254 / #130 (board write caller).**
- *Kept:* peer derivation through `/proc/net/tcp` and pane ancestry; the
  fail-closed `caller_identity` refusal; pane attribution for
  agent-allowed writes.
- *Changed, intentionally:* the `docs/BOARD.md` row "tied to no pane →
  `operator (ui)`" becomes "refused without a session". CAD-254's
  acceptance item "a genuine operator browser write still works on
  loopback and tailnet" now holds **after a one-time login per device**,
  not with zero setup. That is a behaviour change the operator is asked
  to accept (it follows from accepting the ADR).

**CAD-263 / CAD-276 (the pty-tie residual).**
The pty tie was kept because dropping it sent a pane's `setsid` children
back to `operator (ui)`, which is an escalation. Under this ADR, dropping
it sends them to a *refusal* instead. The argument for keeping a
forgeable signal therefore weakens. This ADR does not change the tie.
CAD-280 item 3 should re-decide it with this in mind.

**CAD-292 / #162 (`operator_proof` for force-release).**
No conflict. `rollout release --force` keeps working with the daemon
stopped. §5.7 adds a secret-possession check that is also daemon-free
(it reads the file), subject to Q7. One doc nuance: `operator_proof`
stops being described as "the operator gate". It becomes "the process
gate that accompanies possession".

**ADR 0001 §1.4** ("a guardrail against agent error, not a hostile local
process"). Phase 1 stays within that framing: it ends *ambient*
authority, not *stealable* authority. Phase 2 is the first place cadence
claims more. It must then say which deputies (§5.11) remain.

**`docs/AUDIT.md` "unverified until CAD-280".** Approvals recorded
through a web session or the secret-gated CLI could later carry
`verified: session|cli-proof`. That is out of scope here. It is noted so
the audit wording is revisited when phase 1 lands.

## 9. Consequences

**Good.**
- F1 to F3 close.
- Operator authority becomes one rule with one authority (the daemon),
  shared by the board and the CLI.
- CAD-309's operator-only surfaces can be built on a class that fails
  closed.
- The design works unchanged under OS-user separation and on macOS
  (possession does not need `/proc`).
- The phase-2 hook exists from day one.

**Bad, and accepted.**
- The operator logs in once per device per week.
- Scripts that `curl` the board as the operator must move to the CLI
  (`observe` mode finds them).
- The daemon becomes a hard dependency of operator *writes* on the board
  (it already is for `registered_panes`).
- XSS in the SPA becomes a session-riding risk, not just a nuisance.

**Ugly.**
- Until phase 2, a same-uid agent that sets out to be the operator can
  be: it reads a file and runs `setsid -f`. Phase 1 makes that
  deliberate, visible (§5.10) and pinned by a test. It does not make it
  impossible, and **this ADR must not be quoted as saying otherwise.**

## 10. Open questions for the operator

- **Q1 — Is phase 1 enough for acceptance item 2?** "Agent callers
  cannot reach operator-only endpoints even from loopback" is met for
  every caller that does not deliberately steal the secret. Do you
  accept phase 1 with the pinned residual as satisfying item 2, with
  phase 2 as a follow-up under CAD-280? Or must phase 2 (confinement)
  land inside CAD-313? *Recommendation: accept phase 1 for CAD-313, and
  give phase 2 a separate ticket under CAD-280.*
- **Q2 — Agent writes through the board.** Keep issue writes
  agent-allowed (attributed, CLI parity, the CAD-254 behaviour)? Or make
  *every* board write operator-only, with agents using the CLI? The
  second is simpler and removes the pane-attribution dependency from the
  board. It breaks agents that QA the board by writing through it.
  *Recommendation: keep agent-allowed.*
- **Q3 — Lifetimes.** Link 120 s. Session idle 24 h, absolute 7 days.
  Acceptable?
- **Q4 — Tailnet reads.** Reads stay public-read on loopback, because
  agents can read the same files anyway. On the tailnet, should reads
  also require a session, since tailnet peers are the only readers for
  whom the data is new? And what does `tailscale start --read-only`
  (the browse-only share) mean then? *Recommendation: require a session
  for tailnet reads by default, and keep `--read-only` as the explicit
  no-login share.*
- **Q5 — Tailnet login allowlist.** Should a verified tailnet login in
  an `operator_logins` list open a session without a link? This depends
  on the TS3 hairpin result. *Recommendation: no, at least until TS3
  shows hairpin traffic does not carry the owner's login.*
- **Q6 — Rollout and monitor ack.** Ship one release in `observe` mode
  (the F1 hole stays open for that release, and is logged), or enforce
  immediately? Separately, should `POST /api/monitors/…/ack` be
  operator-only instead of agent-allowed?
- **Q7 — The CLI secret check.** Add the secret-possession check to the
  *existing* operator verbs (`slot reconcile`, `approval record/revoke`,
  `rollout release --force`) in CAD-313? Or only to the new ones
  (`ui login`, `ui sessions`, `model_defaults_set`), leaving the rest to
  CAD-280?
- **Q8 — Sandbox boards.** Under `CADENCE_PROFILE=sandbox:*`, may an
  agent mint a *sandbox* login (skipping `operator_proof`), so that
  agents can QA the UI end to end? The sandbox has its own state dir and
  secret, so production authority is unaffected.
- **Q9 — No legacy switch.** Confirm that there is **no**
  "legacy loopback operator" escape hatch, and that break-glass is the
  CLI plus `--read-only`.
- **Q10 — Where the secret lives.** A file under the state dir
  (proposed), or the OS keychain (macOS Keychain, Secret Service)? The
  keychain adds no protection against the same uid on Linux (an unlocked
  keyring serves any same-uid process), but it is idiomatic on macOS.
  *Recommendation: the file, with the keychain as a CAD-315 option.*

## 11. Acceptance checks for this ADR

These are what a reviewer checks before marking the ADR accepted. The
implementation's own checks are in §7.

```bash
# The ADR exists, is accepted, and is indexed
test -f docs/adr/0004-operator-identity-web-ui.md
grep -q 'Status: \*\*accepted' docs/adr/0004-operator-identity-web-ui.md
grep -q 'adr/0004-operator-identity-web-ui.md' docs/START-HERE.md

# Every current route in src/ui.rs is classified in §5.9. List the route
# literals, assert the list is non-empty (CAD-138), then check each.
routes=$(grep -oE '"/api/[a-z/-]+' src/ui.rs | tr -d '"' | sort -u)
test -n "$routes" || { echo "no routes found — check, not pass"; exit 1; }
for r in $routes; do
  grep -F -- "$r" docs/adr/0004-operator-identity-web-ui.md | grep -q '^| ' || echo "unclassified: $r"
done
```

## 12. Implementation record (phase 1, CAD-313 + CAD-428)

This section records how phase 1 was built and where it departs from the
text above. The sections above remain the decision.

**Open questions.** The operator accepted the ADR without answering §10
item by item, so the implementation takes each recommendation, and the
enforcement the acceptance note asked for:

| Q | Taken as |
|---|---|
| Q1 | Phase 1 with the pinned residual (`operator_secret_theft_residual_pinned`); phase 2 stays with CAD-280. |
| Q2 | Issue writes stay agent-allowed (attributed to the pane or managed endpoint). |
| Q3 | Link 120 s; session idle 24 h, absolute 7 days. |
| Q4 | Not changed here: reads stay as they are on every origin. Tailnet reads behind a session is a follow-up. |
| Q5 | No tailnet login allowlist. |
| Q6 | **Enforce at once, no `observe` mode.** The acceptance note (CAD-313 comment, 2026-09-24) requires the caller without a session to be refused, and CAD-428 requires the relay to be refused, so an `observe` release would ship both holes open. Monitor ack stays agent-allowed. |
| Q7 | The secret check is added to the new verbs only (`ui login`, `ui sessions`, `ui login --rotate`). |
| Q8 | No sandbox exception: a sandbox board mints like any other. |
| Q9 | No legacy loopback switch. |
| Q10 | The file. |

**What was built.**

- `src/operator_auth.rs`: the secret (strict modes on every read, never
  repaired), the link nonces (memory only) and the sessions
  (`operator/sessions.json`, hashes only, `0600`).
- `src/daemon/operator_rpc.rs`: `operator_link_mint`,
  `operator_session_open`, `operator_session_check`,
  `operator_session_logout`, `operator_session_stolen`,
  `operator_sessions`, `operator_secret_rotate`.
- `src/ui/operator.rs`: the route table (`WRITE_ROUTES`, unlisted writes
  are operator-only), the caller rule as a pure table (`decide`), the
  cookie and `POST /api/session[/logout]`, and `/api/meta`'s `operator`,
  `session` and `login_hint`.
- `cadence ui login [--tailnet] [--rotate] [--port] [--json]` and
  `cadence ui sessions [--revoke <id>] [--revoke-all] [--json]`.

**Departures.**

- Credentials are 32 random bytes as lowercase hex, not base64url.
- The cookie name carries the board port (`cadence_operator_<port>`,
  `__Host-cadence_operator_<port>` on the tailnet), so two boards on one
  host (production and a sandbox) do not overwrite each other's cookie.
- A cookie-bearing write must send `Origin`, and it must equal the
  request's own scheme and Host, not merely an allowlisted origin.
- Operator-only routes keep the positive process proof on the HTTP peer
  (`prove_operator_peer`) in addition to the session. The §5.6 table
  alone would grant `Err`-attributed peers; the repo rule is that the
  board is never less strict than the daemon verb it relays. The cost:
  an operator browser whose peer fails `operator_proof` (another uid's
  relay) is refused on those routes and uses the CLI.
- A session is bound to its origin at the daemon. A request whose Host
  names the tailnet but fails the tailnet proof has no origin, so no
  session can be used or opened on it.
- Not built in this phase: the inotify detection of §5.10 (the
  `operator_link_rejected` and `operator_session_from_agent` daemon
  events are recorded with `alert: true`, but are not yet projected onto
  the Overview), `--qr` and `--open`, and `cadence setup` printing the
  link (CAD-312). `ui start` prints `sign_in: cadence ui login`.
- The daemon operator RPCs behind the board (`plan_approve`,
  `delivery_decline`, `model_defaults_set`) keep their existing
  connection gate (`operator_connection`, CAD-337) and do not also take
  the forwarded session.
- TS3 (the hairpin probe) was not run: it needs the real tailnet, which
  this lane may not touch. It remains for the security review.
