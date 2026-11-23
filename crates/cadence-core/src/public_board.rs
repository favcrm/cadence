use serde::{Deserialize, Serialize};

/// CAD-526: this board's AgenticOS public identity as `ui start`
/// recorded it — all four fields together or none.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
pub struct PublicBoard {
    /// The board's public host — `aud` must equal exactly this
    /// (hostname plus port when present, e.g. `acme.board.localhost:3123`
    /// locally or `acme.cadencecloud.app` in production).
    pub host: String,
    /// The platform issuer — the JWS `iss`, and the origin the JWKS is
    /// fetched from (`{iss}/.well-known/agenticos-board-jwks.json`).
    pub issuer: String,
    /// The company/workspace id this instance serves — `company` must
    /// equal it; one Cadence serves one company (contract §9).
    pub company: String,
    /// Where an absent or expired browser session redirects:
    /// `{app}/v2/board/authorize` on the app origin.
    pub authorize_url: String,
}
