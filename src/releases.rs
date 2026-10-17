//! CAD-1455: the owned HTTPS release source (AOS-255 contract).
//!
//! An operator may configure an owned release endpoint (the standalone
//! read-only AOS releases worker) so `cadence update`/`upgrade` consumes
//! an already-published approved release without the GitHub API. The
//! default stays the existing GitHub route; this source is used only
//! when the operator passes its base URL explicitly. A configured
//! source that fails REFUSES — there is never a silent fallback to the
//! GitHub API inside one run.
//!
//! The catalogue is discovery and custody, never executable authority:
//! the endpoint's unsigned JSON selects WHICH published subject to
//! fetch; the installed bytes are authorized only by
//! [`OfflineGh`](crate::upgrade::OfflineGh) — the captured binary,
//! receipt, build/promotion/completion Sigstore bundles and the
//! producer-signed publication-time source facts. That snapshot answers
//! on-main/ancestry/schema exactly as the producer observed them at
//! publication; live-only queries (run lists, artifact state,
//! downloads, live attestation) refuse rather than invent answers, and
//! a pinned `--sha`/`--to` the catalogue does not publish refuses —
//! the catalogue cannot stand in for a live eligibility lookup.
//!
//! Every response is bounded, every name is parsed through the strict
//! shared schema, and every fetch path is re-minted from the parsed
//! dimensions on the configured same-origin base — never from a
//! URL the endpoint (or any catalogue field) hands back.

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::upgrade::{
    is_full_sha, ArtifactState, Job, OfflineGh, OnMain, ReleaseSource, RetainedProof, Run,
    SourceFacts, MANIFEST, SHA_FILE,
};

/// The catalogue/API prefix the standalone worker serves (contract:
/// `CADENCE_RELEASES_PREFIX`). Requests are built from this constant,
/// never from endpoint-supplied paths.
pub const CADENCE_RELEASES_PREFIX: &str = "/v1/cadence";
/// The product dimension this source serves.
pub const PRODUCT_CADENCE: &str = "cadence";
/// The operator-host profile (managed daemon, promotion-gated).
pub const PROFILE_OPERATOR_HOST: &str = "operator-host";
/// The public-client profile (signed tag package — a distinct release
/// authority that never substitutes for operator-host approval).
pub const PROFILE_PUBLIC_CLIENT: &str = "public-client";

/// The default owned release source (CAD-1479): ordinary
/// `cadence update`/`upgrade` and the client-only updater consume the
/// published release through this catalogue's signed offline evidence
/// instead of live GitHub API queries. `--release-source` remains the
/// advanced https:// override; a source failure refuses — there is no
/// GitHub API fallback.
pub const DEFAULT_BASE: &str = "https://api-v2.agenticos.hk";

/// Resolve the release-source base: an explicit `--release-source`
/// wins, else the [`DEFAULT_BASE`] catalogue. Under `test-seam` only,
/// a `CADENCE_RELEASE_SOURCE` env names the seam default — an
/// `http://127.0.0.1:<port>` origin takes the same loopback
/// constructor the acceptance fixtures use, every other value the
/// production https-origin path. The feature compiles out of every
/// release build, so no env can redirect a production binary's
/// default source.
pub fn resolve_base(explicit: Option<&str>) -> Result<String> {
    if let Some(base) = explicit {
        return parse_origin(base);
    }
    #[cfg(feature = "test-seam")]
    if let Ok(env_base) = std::env::var("CADENCE_RELEASE_SOURCE") {
        let env_base = env_base.trim().to_string();
        if !env_base.is_empty() {
            if env_base.starts_with("http://127.0.0.1:") {
                return Ok(env_base);
            }
            return parse_origin(&env_base);
        }
    }
    Ok(DEFAULT_BASE.to_string())
}

/// Construct the source for a resolved base: the loopback seam
/// constructor for a `http://127.0.0.1:<port>` default under
/// `test-seam` (refused in production), `for_profile` otherwise.
pub fn source_for(base: &str, profile: &'static str) -> Result<HttpGh> {
    #[cfg(feature = "test-seam")]
    if base.starts_with("http://127.0.0.1:") {
        return HttpGh::for_profile_loopback(base, profile);
    }
    HttpGh::for_profile(base, profile)
}

/// Metadata responses are small closed documents; larger bodies are
/// refused unread.
const METADATA_MAX_BYTES: u64 = 256 * 1024;
/// The release package tarball holds the retained evidence plus the
/// bounded binary (per-member bounds cap every member's accepted
/// bytes before it is written, and `capture_bounded` enforces them
/// once more inside the verifier).
const ARTIFACT_MAX_BYTES: u64 = 768 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// Members the operator-host release package may hold — exactly the
/// retained evidence files the verifier captures.
const PACKAGE_MAX_MEMBERS: usize = 16;
/// The package's `tar -tvz` listing is a few dozen short lines; a
/// listing beyond this refuses unread rather than trusting a flood.
const LISTING_MAX: usize = 16 * 1024;

/// The operator-host release package: one gzipped tar of the flat
/// retained package (binary, checksum, manifest, receipt, the three
/// bundles, the signed source facts).
pub const PACKAGE_ARCHIVE: &str = "cadence-release.tar.gz";
/// The public-client tarball is the exact CI release asset name
/// `cadence-<tag>-<target>.tar.gz`; its offline build bundle is
/// `build-attestation-<target>.jsonl`, and `<asset>.sha256` the exact
/// published checksum file.
pub fn client_bundle_name(target: &str) -> String {
    format!("build-attestation-{target}.jsonl")
}

/// A lowercase sha256 the contract spells `sha256:<64 hex>`.
fn is_contract_digest(s: &str) -> bool {
    s.len() == 71
        && s.starts_with("sha256:")
        && s[7..]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// One safe-ASCII artifact name — the contract's
/// `releaseArtifactNameSchema`: 1..=160 chars, leading alphanumeric,
/// no separators or `..`.
pub fn is_artifact_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !s.contains("..")
}

/// One safe-ASCII version segment — the contract's
/// `releaseVersionSchema`: 1..=80 chars, never dots-only or traversal.
fn is_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
        && !s.contains("..")
        && !s.bytes().all(|b| b == b'.')
}

/// A real ISO-8601 instant (`YYYY-MM-DDTHH:MM:SS(.f+)?(Z|±HH:MM)`),
/// bounded — the contract parses it as a real instant; a nonempty
/// arbitrary string is malformed.
fn is_iso_datetime(s: &str) -> bool {
    let b = s.as_bytes();
    if !(20..=40).contains(&b.len()) {
        return false;
    }
    if b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || !b[0..4]
            .iter()
            .chain(&b[5..7])
            .chain(&b[8..10])
            .chain(&b[11..13])
            .chain(&b[14..16])
            .chain(&b[17..19])
            .all(u8::is_ascii_digit)
    {
        return false;
    }
    let mut rest = &b[19..];
    if rest.first() == Some(&b'.') {
        let frac = rest[1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if frac == 0 {
            return false;
        }
        rest = &rest[1 + frac..];
    }
    match rest {
        [b'Z'] => true,
        [sign, h1, h2, b':', m1, m2] => {
            matches!(sign, b'+' | b'-')
                && h1.is_ascii_digit()
                && h2.is_ascii_digit()
                && m1.is_ascii_digit()
                && m2.is_ascii_digit()
        }
        _ => false,
    }
}

/// The strict catalogue entry the worker serves (contract:
/// `releaseEntrySchema`). `deny_unknown_fields` mirrors `strictObject`;
/// a malformed or future-fattened entry refuses to parse.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseArtifact {
    pub name: String,
    pub profile: String,
    pub platform: String,
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "contentType")]
    pub content_type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseEntry {
    pub id: String,
    pub product: String,
    pub version: String,
    #[serde(rename = "sourceSha")]
    pub source_sha: String,
    pub profile: String,
    pub visibility: String,
    pub status: String,
    #[serde(rename = "publishedAt")]
    pub published_at: Option<String>,
    #[serde(rename = "withdrawnAt")]
    pub withdrawn_at: Option<String>,
    #[serde(rename = "recordedAt")]
    pub recorded_at: String,
    #[serde(rename = "productionCandidateId")]
    pub production_candidate_id: Option<String>,
    pub artifacts: Vec<ReleaseArtifact>,
}

/// The entry's own closed consistency rules (the contract's
/// `superRefine`): `id` is exactly `product/profile/version`, artifact
/// names are unique and bound to the entry's profile.
fn entry_consistent(entry: &ReleaseEntry) -> bool {
    entry.id == format!("{}/{}/{}", entry.product, entry.profile, entry.version)
        && entry.id.len() <= 240
        && is_version(&entry.version)
        && is_full_sha(&entry.source_sha)
        && matches!(entry.visibility.as_str(), "public" | "private")
        && matches!(entry.status.as_str(), "published" | "withdrawn")
        && (entry.status != "withdrawn" || entry.withdrawn_at.is_some())
        && is_iso_datetime(&entry.recorded_at)
        && entry.published_at.as_deref().is_none_or(is_iso_datetime)
        && entry.withdrawn_at.as_deref().is_none_or(is_iso_datetime)
        && entry
            .production_candidate_id
            .as_deref()
            .is_none_or(|id| !id.is_empty() && id.len() <= 200)
        && entry.artifacts.len() <= 16
        && entry.artifacts.iter().all(|artifact| {
            artifact.profile == entry.profile
                && is_artifact_name(&artifact.name)
                && is_contract_digest(&artifact.sha256)
                && !artifact.platform.is_empty()
                && artifact.platform.len() <= 64
                && matches!(
                    artifact.content_type.as_str(),
                    "application/octet-stream" | "application/json" | "text/plain"
                )
        })
        && {
            let mut names: Vec<&str> = entry
                .artifacts
                .iter()
                .map(|artifact| artifact.name.as_str())
                .collect();
            names.sort();
            names.dedup();
            names.len() == entry.artifacts.len()
        }
}

/// The strict envelopes: `{ok:true,data:{…}}` on 2xx,
/// `{ok:false,error:{code,message}}` on failures.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    ok: bool,
    data: Option<Value>,
    error: Option<EnvelopeError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeError {
    code: String,
    #[allow(dead_code)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatestData {
    product: String,
    profile: String,
    entry: Option<ReleaseEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetailData {
    entry: ReleaseEntry,
    artifacts: Vec<DetailArtifact>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetailArtifact {
    name: String,
    url: String,
}

/// An owned HTTPS release source. `base` is the endpoint's https origin
/// only — no path, query, fragment or credentials — so every request is
/// built on `/v1/cadence/…` on the configured host and can never be
/// redirected by endpoint data (redirects are refused, not followed).
///
/// `proof` holds the release fetched and verified during this run; the
/// signed publication-time facts inside it answer on-main/ancestry/
/// schema. [`ReleaseSource::published_proof`] takes it so the install
/// consumes the identical captured bytes.
pub struct HttpGh {
    base: String,
    profile: &'static str,
    http: ureq::Agent,
    offline: OfflineGh,
    proof: RefCell<Option<RetainedProof>>,
}

impl HttpGh {
    /// Build a source for the operator-host profile. `base` must be an
    /// `https://host[:port]` origin: nothing after the host is
    /// accepted, so a configured URL can never smuggle a path, a query
    /// or credentials into the fetch base.
    pub fn new(base: &str) -> Result<Self> {
        Self::for_profile(base, PROFILE_OPERATOR_HOST)
    }

    /// The same source for another profile (`public-client` is used by
    /// the client update path only, never by the daemon updater).
    pub fn for_profile(base: &str, profile: &'static str) -> Result<Self> {
        if !matches!(profile, PROFILE_OPERATOR_HOST | PROFILE_PUBLIC_CLIENT) {
            return Err(Error::rejected(format!(
                "release profile `{profile}` is not a supported release profile"
            )));
        }
        let origin = parse_origin(base)?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            // Redirects are never followed: every fetch path is minted
            // here, and a redirect would leave the configured origin.
            .max_redirects(0)
            .build();
        Ok(Self {
            base: origin,
            profile,
            http: ureq::Agent::new_with_config(config),
            offline: OfflineGh::new(crate::upgrade::DEFAULT_REPO),
            proof: RefCell::new(None),
        })
    }

    /// The configured profile.
    pub fn profile(&self) -> &'static str {
        self.profile
    }

    /// TEST SEAM ONLY (CAD-482): a loopback constructor for the
    /// independent refusal-acceptance fixtures. `base` must be
    /// `http://127.0.0.1:<port>` — an explicit numeric loopback port,
    /// nothing else; every other origin refuses, so this can never
    /// point the client at a remote host or a plain-`http` name. The
    /// production constructors stay `https://`-only, and the
    /// `test-seam` feature itself cannot compile into a release build
    /// (`compile_error!` in `crate::test_seam`). The constructed source
    /// is otherwise identical: same strict schema, same bounded reads,
    /// same offline verification.
    #[cfg(feature = "test-seam")]
    pub fn for_profile_loopback(base: &str, profile: &'static str) -> Result<Self> {
        if !matches!(profile, PROFILE_OPERATOR_HOST | PROFILE_PUBLIC_CLIENT) {
            return Err(Error::rejected(format!(
                "release profile `{profile}` is not a supported release profile"
            )));
        }
        let uri: ureq::http::Uri = base.parse().map_err(|_| {
            Error::rejected(format!(
                "release source `{base}` is not a valid URL — the test seam accepts \
                 only http://127.0.0.1:<port>"
            ))
        })?;
        let scheme = uri.scheme_str().unwrap_or_default();
        let authority = uri.authority().map(|a| a.as_str()).unwrap_or_default();
        let path = uri.path();
        let host = authority.split(':').next().unwrap_or_default();
        let port = authority
            .split_once(':')
            .map(|(_, p)| p)
            .unwrap_or_default();
        if scheme != "http"
            || host != "127.0.0.1"
            || authority.contains('@')
            || port.is_empty()
            || !port.bytes().all(|b| b.is_ascii_digit())
            || !matches!(path, "" | "/")
            || uri.query().is_some()
            || base.contains('#')
        {
            return Err(Error::rejected(format!(
                "release source `{base}` is not a loopback origin — the test seam \
                 accepts only http://127.0.0.1:<port> (no path, query or credentials)"
            )));
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Ok(Self {
            base: format!("{scheme}://{authority}"),
            profile,
            http: ureq::Agent::new_with_config(config),
            offline: OfflineGh::new(crate::upgrade::DEFAULT_REPO),
            proof: RefCell::new(None),
        })
    }

    /// `GET` one metadata document: 2xx + `{ok:true,data}` or a
    /// refusal. A 4xx `{ok:false}` names the endpoint's own refusal;
    /// any other status, redirect or malformed body refuses the
    /// configured source.
    fn get_envelope(&self, url: &str, what: &str) -> Result<Value> {
        let mut response = self.http.get(url).call().map_err(|e| {
            Error::rejected(format!(
                "the configured release source cannot be read for {what} ({e}) — \
                 refusing; check the endpoint"
            ))
        })?;
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(METADATA_MAX_BYTES)
            .read_to_vec()
            .map_err(|e| {
                Error::rejected(format!("the configured release source's {what} body: {e}"))
            })?;
        let envelope: Envelope = serde_json::from_slice(&bytes)
            .map_err(|e| Error::rejected(format!("the configured release source's {what}: {e}")))?;
        match (status, envelope.ok) {
            (200..=299, true) => envelope.data.ok_or_else(|| {
                Error::rejected(format!(
                    "the configured release source's {what} carries no data — refusing"
                ))
            }),
            (400..=499, false) => Err(Error::rejected(format!(
                "the configured release source refused {what} ({})",
                envelope
                    .error
                    .map(|e| e.code)
                    .unwrap_or_else(|| "error".to_string())
            ))),
            _ => Err(Error::rejected(format!(
                "the configured release source answered HTTP {status} for {what} — refusing"
            ))),
        }
    }

    /// The catalogue detail for `product/profile/version`: one
    /// validated entry or a refusal. `version` only addresses the
    /// fetch; the parsed entry must bind all three dimensions.
    pub fn detail(&self, version: &str) -> Result<ReleaseEntry> {
        if !is_version(version) {
            return Err(Error::rejected(format!(
                "release version `{version}` is not a safe catalogue segment"
            )));
        }
        let url = format!(
            "{}{}/releases/{}/{}/{}",
            self.base,
            CADENCE_RELEASES_PREFIX,
            PRODUCT_CADENCE,
            self.profile,
            encode_segment(version)
        );
        let data = self.get_envelope(&url, "release detail")?;
        let detail: DetailData = serde_json::from_value(data).map_err(|e| {
            Error::rejected(format!("release detail for {version} is malformed: {e}"))
        })?;
        let entry = detail.entry;
        if !entry_consistent(&entry)
            || entry.product != PRODUCT_CADENCE
            || entry.profile != self.profile
            || entry.version != version
        {
            return Err(Error::rejected(format!(
                "release detail for {version} does not bind {PRODUCT_CADENCE}/{} — \
                 refusing the configured source",
                self.profile
            )));
        }
        // Every detail artifact is an allowlisted member of the entry
        // under this prefix; the endpoint's `url` fields are never
        // fetched — the consumer re-mints the same canonical paths.
        let prefix = format!("{CADENCE_RELEASES_PREFIX}/artifacts/");
        let canonical = format!(
            "{prefix}{}/{}/{}/",
            PRODUCT_CADENCE,
            self.profile,
            encode_segment(version)
        );
        for artifact in &detail.artifacts {
            if !is_artifact_name(&artifact.name)
                || !entry.artifacts.iter().any(|a| a.name == artifact.name)
                || artifact.url != format!("{canonical}{}", encode_segment(&artifact.name))
            {
                return Err(Error::rejected(format!(
                    "release detail for {version} lists an artifact outside the \
                     published entry's own canonical allowlist — refusing"
                )));
            }
        }
        Ok(entry)
    }

    /// `latest` for this source's profile: the published entry or
    /// `None` when the catalogue has no published release for the pair.
    pub fn latest(&self) -> Result<Option<ReleaseEntry>> {
        let url = format!(
            "{}{}/releases/latest?product={}&profile={}",
            self.base, CADENCE_RELEASES_PREFIX, PRODUCT_CADENCE, self.profile
        );
        let data = self.get_envelope(&url, "latest release")?;
        let latest: LatestData = serde_json::from_value(data)
            .map_err(|e| Error::rejected(format!("latest release answer is malformed: {e}")))?;
        if latest.product != PRODUCT_CADENCE || latest.profile != self.profile {
            return Err(Error::rejected(
                "latest release answer does not match the requested product/profile — refusing",
            ));
        }
        let Some(entry) = latest.entry else {
            return Ok(None);
        };
        if !entry_consistent(&entry)
            || entry.product != PRODUCT_CADENCE
            || entry.profile != self.profile
            || entry.status != "published"
        {
            return Err(Error::rejected(format!(
                "latest {PRODUCT_CADENCE}/{} release is malformed, withdrawn or not \
                 published — refusing the configured source",
                self.profile
            )));
        }
        Ok(Some(entry))
    }

    /// The entry's artifact record for `name`, bound to `platform`.
    pub fn release_artifact<'a>(
        &self,
        entry: &'a ReleaseEntry,
        name: &str,
        platform: &str,
    ) -> Result<&'a ReleaseArtifact> {
        entry
            .artifacts
            .iter()
            .find(|a| a.name == name)
            .filter(|a| a.platform == platform)
            .ok_or_else(|| {
                Error::rejected(format!(
                    "published {} release {} carries no {name} for {platform} — refusing",
                    entry.profile, entry.version
                ))
            })
    }

    /// `GET` one artifact's bytes to `dest` through a temp file and a
    /// rename, refusing anything that is not an exact match for the
    /// catalogue's recorded digest and size. The path is minted from
    /// the validated dimensions only — never from entry data.
    pub fn download_artifact(
        &self,
        entry: &ReleaseEntry,
        artifact: &ReleaseArtifact,
        dest: &Path,
    ) -> Result<()> {
        if !is_version(&entry.version) || !is_artifact_name(&artifact.name) {
            return Err(Error::rejected(
                "published release names an unsafe fetch segment — refusing",
            ));
        }
        let url = format!(
            "{}{}/artifacts/{}/{}/{}/{}",
            self.base,
            CADENCE_RELEASES_PREFIX,
            PRODUCT_CADENCE,
            self.profile,
            encode_segment(&entry.version),
            encode_segment(&artifact.name)
        );
        let mut response = self.http.get(url).call().map_err(|e| {
            Error::rejected(format!(
                "the configured release source cannot serve {} ({e}) — refusing",
                artifact.name
            ))
        })?;
        if response.status().as_u16() != 200 {
            return Err(Error::rejected(format!(
                "the configured release source answered HTTP {} for {} — refusing",
                response.status().as_u16(),
                artifact.name
            )));
        }
        let dir = dest
            .parent()
            .ok_or_else(|| Error::internal(format!("{} has no parent", dest.display())))?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".release-")
            .tempfile_in(dir)?;
        std::io::copy(
            &mut response
                .body_mut()
                .with_config()
                .limit(ARTIFACT_MAX_BYTES)
                .reader(),
            tmp.as_file_mut(),
        )
        .map_err(|e| {
            Error::rejected(format!(
                "could not store {} from the configured release source: {e}",
                artifact.name
            ))
        })?;
        tmp.as_file().sync_all()?;
        tmp.persist(dest).map_err(|e| Error::from(e.error))?;
        let meta = fs::metadata(dest)?;
        if meta.len() != artifact.size {
            let _ = fs::remove_file(dest);
            return Err(Error::rejected(format!(
                "{} is {} bytes, the published record says {} — refusing",
                artifact.name,
                meta.len(),
                artifact.size
            )));
        }
        let digest = format!("sha256:{}", crate::upgrade::sha256_file(dest)?);
        if digest != artifact.sha256 {
            let _ = fs::remove_file(dest);
            return Err(Error::rejected(format!(
                "{} hashes to {digest}, the published record says {} — refusing",
                artifact.name, artifact.sha256
            )));
        }
        Ok(())
    }

    /// Fetch the published operator-host package into an owned staging
    /// dir and prove it offline. The tarball is never extracted to the
    /// filesystem: its members are read through bounded `tar -xOzf`
    /// stdout and written create-new into a private flat dir, and only
    /// that accepted set reaches `verify_retained`. See the member
    /// admission below for the exact rules.
    fn fetch_and_prove(&self, entry: &ReleaseEntry, expect_sha: Option<&str>) -> Result<()> {
        if let Some(expect) = expect_sha {
            if entry.source_sha != expect {
                return Err(Error::rejected(format!(
                    "published release {} names source {}, not the requested {expect} — \
                     refusing",
                    entry.version, entry.source_sha
                )));
            }
        }
        let package = self.release_artifact(entry, PACKAGE_ARCHIVE, crate::upgrade::TARGET)?;
        let dir = tempfile::Builder::new()
            .prefix("cadence-release-")
            .tempdir()?;
        let archive = dir.path().join(PACKAGE_ARCHIVE);
        self.download_artifact(entry, package, &archive)?;
        // The archive is not authenticated yet, so it is never
        // extracted to the filesystem: a verbose listing (bounded in
        // time and retained bytes) must first show only the known flat
        // regular-file member set — an unknown, duplicate or nonregular
        // member (link, device, dir other than the lone container
        // header) refuses before any byte is written. Each accepted
        // member is then read through `tar -xOzf` into a bounded stdout
        // buffer (truncation refuses) and only that accepted buffer is
        // written, create-new, into the private flat directory.
        let archive_arg = archive.to_string_lossy().into_owned();
        let listing = run_tar_bounded(&["-tvzf", &archive_arg], LISTING_MAX)?;
        let listing = String::from_utf8_lossy(&listing);
        let mut members: Vec<(String, String)> = Vec::new();
        for line in listing.lines() {
            let stored = line.split_whitespace().last().unwrap_or_default();
            let name = stored.trim_start_matches("./");
            let kind = line.as_bytes().first().copied();
            // The producer's untrusted transport copy of the trusted
            // root rides along; it is never verified from (the
            // verifier's own embedded snapshot is the anchor) and is
            // simply not extracted into the package the verifier sees.
            if name == "trusted_root.jsonl" {
                if kind != Some(b'-') {
                    return Err(Error::rejected(
                        "release package member trusted_root.jsonl is not a regular file \
                         — refusing",
                    ));
                }
                continue;
            }
            if (name.is_empty() || name == ".") && kind == Some(b'd') {
                // A single ordinary `.`/`./` container-directory
                // header is the only non-file entry ignored; a link
                // never is.
                continue;
            }
            if kind != Some(b'-') || member_bound(name).is_none() {
                return Err(Error::rejected(format!(
                    "release package holds an unexpected member `{stored}` — refusing"
                )));
            }
            if members.iter().any(|(_, member)| member == name) {
                return Err(Error::rejected(format!(
                    "release package lists member `{name}` twice — refusing"
                )));
            }
            members.push((stored.to_string(), name.to_string()));
        }
        if members.is_empty() || members.len() > PACKAGE_MAX_MEMBERS {
            return Err(Error::rejected(
                "release package does not hold the expected bounded member set — refusing",
            ));
        }
        let flat = dir.path().join("package");
        fs::create_dir(&flat)?;
        for (stored, name) in &members {
            let bound = member_bound(name).expect("listed members are bounded");
            let bytes = run_tar_bounded(&["-xOzf", &archive_arg, stored], bound as usize)?;
            if bytes.is_empty() {
                return Err(Error::rejected(format!(
                    "release package member {name} is empty — refusing"
                )));
            }
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(flat.join(name))
                .and_then(|mut file| {
                    use std::io::Write;
                    file.write_all(&bytes)?;
                    file.sync_all()
                })
                .map_err(|e| Error::rejected(format!("release package member {name}: {e}")))?;
        }
        let proof = self.offline.verify_retained(&flat)?;
        if let Some(expect) = expect_sha {
            if proof.source_sha != expect {
                return Err(Error::rejected(format!(
                    "verified release package binds source {}, not the requested \
                     {expect} — refusing",
                    proof.source_sha
                )));
            }
        }
        *self.proof.borrow_mut() = Some(proof);
        Ok(())
    }

    /// The signed facts of the fetched release, when one is stored.
    fn facts(&self) -> Result<(String, SourceFacts)> {
        self.proof
            .borrow()
            .as_ref()
            .map(|p| (p.source_sha.clone(), p.facts.clone()))
            .ok_or_else(|| {
                Error::rejected(
                    "no published release has been resolved yet — the signed \
                     publication-time snapshot answers only what it recorded",
                )
            })
    }

    /// Fetch and prove the release for `sha` if it is not the stored
    /// one: the operator-host catalogue keys a release by its source
    /// SHA (`version == source_sha`), so a pinned target names the
    /// exact detail subject.
    fn ensure(&self, sha: &str) -> Result<()> {
        if self
            .proof
            .borrow()
            .as_ref()
            .is_some_and(|p| p.source_sha == sha)
        {
            return Ok(());
        }
        if self.profile != PROFILE_OPERATOR_HOST {
            return Err(Error::rejected(
                "the public-client profile is consumed by the client update path, \
                 not the daemon updater",
            ));
        }
        if !is_full_sha(sha) {
            return Err(Error::rejected(format!(
                "release target `{sha}` is not a full commit id"
            )));
        }
        let entry = self.detail(sha)?;
        if entry.status != "published" {
            return Err(Error::rejected(format!(
                "release {sha} is {} in the configured source — refusing",
                entry.status
            )));
        }
        self.fetch_and_prove(&entry, Some(sha))
    }
}

/// The flat member set's bound per file — the same bounds
/// `capture_bounded` enforces inside the verifier, applied here before
/// extraction is trusted.
fn member_bound(member: &str) -> Option<u64> {
    use crate::upgrade::{
        BINARY_MAX, BUNDLE_MAX, MANIFEST_MAX, RECEIPT_MAX, SHA_FILE_MAX, SOURCE_FACTS_MAX,
    };
    Some(match member {
        crate::upgrade::BINARY => BINARY_MAX,
        SHA_FILE => SHA_FILE_MAX,
        MANIFEST => MANIFEST_MAX,
        crate::upgrade::RECEIPT_FILE | crate::upgrade::COMPLETION_FILE => RECEIPT_MAX,
        crate::upgrade::PROMOTION_BUNDLE
        | crate::upgrade::BUILD_BUNDLE
        | crate::upgrade::COMPLETION_BUNDLE => BUNDLE_MAX,
        crate::upgrade::SOURCE_FACTS_FILE => SOURCE_FACTS_MAX,
        _ => return None,
    })
}

/// Run `tar` bounded in time AND retained output; a non-zero exit or
/// an output that exceeds `limit` is a refusal — the archive is not
/// authenticated yet, so neither a runaway listing nor an oversized
/// member may write or buffer beyond its bound.
fn run_tar_bounded(args: &[&str], limit: usize) -> Result<Vec<u8>> {
    let (out, bounds) = crate::proc::run_bounded_limited(
        std::process::Command::new("tar").args(args),
        Duration::from_secs(120),
        limit,
    )
    .map_err(|e| Error::rejected(format!("tar on the release package failed: {e}")))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "tar on the release package failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if bounds.stdout_exceeded || bounds.stderr_exceeded {
        return Err(Error::rejected(format!(
            "tar on the release package produced output beyond its {limit}-byte bound \
             — refusing"
        )));
    }
    Ok(out.stdout)
}

/// `encodeURIComponent` for one path segment, matching how the worker
/// mints artifact URLs: the schema's safe-ASCII members pass through
/// except `+`, which becomes `%2B`. Applied to already-validated
/// segments only — never a URL the endpoint handed back.
fn encode_segment(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b == b'+' {
                "%2B".to_string()
            } else {
                (b as char).to_string()
            }
        })
        .collect()
}

/// `https://host[:port]` and nothing else. The origin's authority must
/// carry no userinfo; a path beyond `/`, a query or a fragment refuses.
fn parse_origin(base: &str) -> Result<String> {
    let uri: ureq::http::Uri = base.parse().map_err(|_| {
        Error::rejected(format!(
            "release source `{base}` is not a valid URL — pass an https:// origin"
        ))
    })?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let authority = uri.authority().map(|a| a.as_str()).unwrap_or_default();
    let path = uri.path();
    if scheme != "https"
        || authority.is_empty()
        || authority.contains('@')
        || !matches!(path, "" | "/")
        || uri.query().is_some()
        || base.contains('#')
    {
        return Err(Error::rejected(format!(
            "release source `{base}` is not a plain https:// origin — pass the \
             endpoint's https base only (no path, query or credentials)"
        )));
    }
    Ok(format!("{scheme}://{authority}"))
}

impl ReleaseSource for HttpGh {
    fn repo(&self) -> &str {
        self.offline.repo()
    }

    fn check_auth(&self) -> Result<()> {
        // The owned endpoint needs no GitHub login; `gh` is required
        // only for the offline bundle verification inside the proof.
        Ok(())
    }

    /// A published source fabricates no live run record: the
    /// catalogue's published-subject selection is answered by
    /// [`ReleaseSource::published_source`], and the callers that want
    /// a live run route to it through `published()`.
    fn latest_green_main(&self) -> Result<Option<Run>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    /// The catalogue's latest published operator-host entry selects
    /// the release's recorded source SHA — the explicit
    /// published-subject selection this source makes. The package is
    /// fetched and proven here so the snapshot's answers below bind
    /// the same verified subject.
    fn published_source(&self) -> Result<Option<String>> {
        if self.profile != PROFILE_OPERATOR_HOST {
            return Err(Error::rejected(
                "the public-client profile is consumed by the client update path, \
                 not the daemon updater",
            ));
        }
        let Some(entry) = self.latest()? else {
            return Ok(None);
        };
        if !is_full_sha(&entry.source_sha) {
            return Err(Error::rejected(format!(
                "published release {} names source `{}`, not a full commit id — \
                 refusing",
                entry.version, entry.source_sha
            )));
        }
        self.fetch_and_prove(&entry, Some(&entry.source_sha.clone()))?;
        Ok(Some(entry.source_sha.clone()))
    }

    /// The signed publication-time snapshot: the producer proved the
    /// candidate an ancestor of the main head it observed, so the
    /// published source answers Yes only for its own candidate.
    fn on_main(&self, sha: &str) -> Result<OnMain> {
        // A pinned `--sha`/`--to` that is not the stored release fetches
        // and proves its own published subject here; a sha the
        // catalogue does not publish fails inside `ensure`.
        self.ensure(sha)?;
        let (source, _) = self.facts()?;
        if sha == source {
            Ok(OnMain::Yes)
        } else {
            Ok(OnMain::No(
                "not the published release's recorded source".into(),
            ))
        }
    }

    /// `compare(base, head)`: the signed facts prove ancestry only
    /// relative to their own candidate — `ancestors` is the
    /// candidate's rev-list, so membership answers "ancestor of the
    /// candidate", never an ordering between two sibling ancestors.
    /// `ahead` only when `head` is the candidate and `base` one of its
    /// ancestors, `behind` for the converse, `identical` when both are
    /// the candidate; every other pair answers `None`, and the caller
    /// fails closed exactly as it does when GitHub cannot answer.
    fn compare(&self, base: &str, head: &str) -> Result<Option<String>> {
        let (source, facts) = self.facts()?;
        let base_is_candidate = base == source;
        let head_is_candidate = head == source;
        let base_ancestor = facts.ancestors.iter().any(|a| a == base);
        let head_ancestor = facts.ancestors.iter().any(|a| a == head);
        Ok(match (base_is_candidate, head_is_candidate) {
            (true, true) => Some("identical".to_string()),
            // head is the candidate: it descends from base exactly
            // when base is one of its recorded ancestors.
            (false, true) => base_ancestor.then(|| "ahead".to_string()),
            // base is the candidate: head descends from it only if
            // head is a recorded ancestor (impossible — the candidate
            // is its own list's tip) or the same commit, so any other
            // recorded ancestor of the candidate is behind it.
            (true, false) => head_ancestor.then(|| "behind".to_string()),
            _ => None,
        })
    }

    fn main_runs(&self, _sha: &str) -> Result<Vec<Run>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn merge_group_runs(&self, _sha: &str) -> Result<Vec<Run>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn jobs(&self, _run_id: u64) -> Result<Vec<Job>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn artifact(&self, _run_id: u64, _name: &str) -> Result<ArtifactState> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn download(&self, _run_id: u64, _name: &str, _dest: &Path) -> Result<()> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn verify_attestation(&self, _binary: &Path, _sha: &str) -> Result<String> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    /// This source installs published, verified packages.
    fn published(&self) -> bool {
        self.profile == PROFILE_OPERATOR_HOST
    }

    /// Resolve/fetch/prove the published release for `sha` (or reuse
    /// the one `latest_green_main` already proved) and hand the
    /// captured workspace to the install step — the identical verified
    /// bytes, never re-downloaded.
    fn published_proof(&self, sha: &str) -> Result<RetainedProof> {
        self.ensure(sha)?;
        self.proof
            .borrow_mut()
            .take()
            .ok_or_else(|| Error::rejected("no published release has been proven in this run"))
    }

    /// `verify_retained` on this source is the fetched published
    /// proof; the `dir` argument is the OfflineGh package seam and is
    /// unused here.
    fn verify_retained(&self, dir: &Path) -> Result<RetainedProof> {
        let _ = dir;
        self.proof.borrow_mut().take().ok_or_else(|| {
            Error::rejected(
                "no published release has been resolved — fetch it through \
                 latest_green_main/on_main/published_proof first",
            )
        })
    }

    fn merged_titles(&self, _base: &str, _head: &str) -> Result<Vec<String>> {
        // The snapshot carries no PR titles; `--check` reports none.
        Ok(Vec::new())
    }

    /// `SCHEMA_VERSION` at `sha` when the signed facts name it — the
    /// publication-time snapshot's own record, never a contents call.
    fn schema_version(&self, sha: &str) -> Result<Option<i64>> {
        // Resolve the pinned sha the same way `on_main` does so a
        // `--check --to <sha>` answers from that release's own facts.
        self.ensure(sha)?;
        let (source, facts) = self.facts()?;
        if sha == source {
            Ok(Some(facts.schema_version))
        } else {
            Ok(None)
        }
    }
}

const OFFLINE_NO_LIVE: &str =
    "a published release source answers no live GitHub queries — refusing; \
     the signed publication-time snapshot answers only what it recorded";
