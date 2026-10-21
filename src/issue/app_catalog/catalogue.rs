//! Catalogue locator as installation provenance.
//!
//! The locator names a published release. It is not publisher trust, not a
//! permission, and not an instruction to update. Bytes, identity and the
//! closed directory metadata have to agree before it is stored.
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::app::CatalogueOrigin;

/// What an install or upgrade request said about catalogue provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CatalogueClaim {
    /// The field was omitted. A fresh install or a non-catalogue update
    /// records no origin. Omitting it on a catalogue-origin install refuses.
    Absent,
    /// The operator named a non-catalogue source. Stored origin is cleared.
    Manual,
    /// A locator to check against this package and directory metadata.
    Locator(CatalogueOrigin),
}

/// Where versions metadata is read. `Configured` is the process catalog
/// origin. `Origin` is only for a test that already bound a loopback
/// directory; it is not a request field.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CatalogueDirectory<'a> {
    Configured,
    /// Only the workspace catalogue tests construct this — the variant
    /// exists so a bound loopback directory can stand in for the
    /// configured origin; it is never a request field.
    #[allow(dead_code)]
    Origin(&'a str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Provenance {
    pub(super) origin: Option<CatalogueOrigin>,
    pub(super) manual: bool,
}

pub(crate) fn claim_from_object(object: &serde_json::Map<String, Value>) -> Result<CatalogueClaim> {
    let catalogue = object.get("catalogue");
    let resolution = object.get("catalogue_resolution");
    if catalogue.is_some() && resolution.is_some() {
        return Err(Error::rejected(
            "catalogue locator and manual resolution are mutually exclusive",
        ));
    }
    if let Some(resolution) = resolution {
        if resolution.as_str() != Some("manual") {
            return Err(Error::rejected(
                "catalogue resolution must be the string manual",
            ));
        }
        return Ok(CatalogueClaim::Manual);
    }
    let Some(catalogue) = catalogue else {
        return Ok(CatalogueClaim::Absent);
    };
    let Some(fields) = catalogue.as_object() else {
        return Err(Error::rejected("catalogue locator must be an object"));
    };
    if fields.len() != 3
        || !matches!(
            (
                fields.get("id").and_then(Value::as_str),
                fields.get("version").and_then(Value::as_str),
                fields.get("digest").and_then(Value::as_str),
            ),
            (Some(_), Some(_), Some(_))
        )
    {
        return Err(Error::rejected(
            "catalogue locator admits only id, version and digest",
        ));
    }
    let locator = CatalogueOrigin {
        id: fields["id"].as_str().unwrap().to_string(),
        version: fields["version"].as_str().unwrap().to_string(),
        digest: fields["digest"].as_str().unwrap().to_string(),
    };
    locator_shape(&locator)?;
    Ok(CatalogueClaim::Locator(locator))
}

fn locator_shape(locator: &CatalogueOrigin) -> Result<()> {
    if !catalogue_id_ok(&locator.id)
        || !digest_ok(&locator.digest)
        || !version_bound(&locator.version)
    {
        return Err(Error::rejected(
            "catalogue locator must be a directory id, an exact version and a sha256 digest",
        ));
    }
    Ok(())
}

pub(crate) fn catalogue_id_ok(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let mut hyphen = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => hyphen = false,
            b'-' if index > 0 && !hyphen && index + 1 < bytes.len() => hyphen = true,
            _ => return false,
        }
    }
    !hyphen && !id.ends_with('-')
}

fn digest_ok(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn version_bound(version: &str) -> bool {
    !version.is_empty() && version.chars().count() <= 40 && !version.chars().any(char::is_control)
}

/// `MAJOR.MINOR.PATCH` with no leading zeros. Anything else is unordered.
fn semver(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = numeric(parts.next()?)?;
    let minor = numeric(parts.next()?)?;
    let patch = numeric(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn numeric(part: &str) -> Option<u64> {
    if part.is_empty() || part.len() > 20 {
        return None;
    }
    if part.len() > 1 && part.starts_with('0') {
        return None;
    }
    if !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// Resolve what the record should store. This does not read the network.
pub(super) fn resolve_provenance(
    existing: Option<&CatalogueOrigin>,
    claim: &CatalogueClaim,
    updating: bool,
    old_version: Option<&str>,
    new_version: &str,
    new_digest: &str,
) -> Result<Provenance> {
    match claim {
        CatalogueClaim::Manual => Ok(Provenance {
            origin: None,
            manual: true,
        }),
        CatalogueClaim::Absent => {
            if updating && existing.is_some() {
                return Err(Error::rejected(
                    "catalogue origin stays until the update names the new release or resolves provenance as manual",
                ));
            }
            Ok(Provenance {
                origin: None,
                manual: false,
            })
        }
        CatalogueClaim::Locator(locator) => {
            locator_shape(locator)?;
            if locator.version != new_version || locator.digest != new_digest {
                return Err(Error::rejected(
                    "catalogue locator does not match this package's version and digest",
                ));
            }
            if let Some(existing) = existing {
                if existing.id != locator.id {
                    return Err(Error::rejected(
                        "catalogue id does not match this installation",
                    ));
                }
            }
            if updating {
                let old_version = old_version
                    .filter(|version| !version.is_empty())
                    .ok_or_else(|| Error::rejected("installed version is missing"))?;
                match (semver(old_version), semver(new_version)) {
                    (Some(old), Some(new)) if new < old => {
                        return Err(Error::rejected(
                            "catalogue update cannot move an installation to an older release",
                        ));
                    }
                    (Some(_), Some(_)) => {}
                    _ => {
                        return Err(Error::rejected(
                            "catalogue versions are not ordered; refusing the update rather than guessing a downgrade",
                        ));
                    }
                }
            }
            Ok(Provenance {
                origin: Some(locator.clone()),
                manual: false,
            })
        }
    }
}

/// The versions document must name this package's app and list this exact
/// version and digest as installable. `recommended_version`, release notes,
/// and any publisher label are not a grant. A missing or unreadable document
/// does not authorize the claim.
pub(super) fn eligible(
    doc: &Value,
    locator: &CatalogueOrigin,
    package_app: &str,
    package_version: &str,
    package_digest: &str,
) -> Result<()> {
    if locator.version != package_version || locator.digest != package_digest {
        return Err(Error::rejected(
            "catalogue locator does not match this package's version and digest",
        ));
    }
    let releases = release_list(doc, &locator.id, package_app)?;
    let release = release_for(releases, package_version)?;
    require_installable(release, package_digest)
}

fn release_list<'a>(doc: &'a Value, id: &str, app: &str) -> Result<&'a [Value]> {
    if doc.as_object().is_none_or(|fields| fields.len() != 4)
        || !matches!(
            doc.get("recommended_version"),
            Some(Value::Null | Value::String(_))
        )
    {
        return Err(Error::rejected("catalogue metadata is unavailable"));
    }
    if doc.get("app_id").and_then(Value::as_str) != Some(id)
        || doc.get("app").and_then(Value::as_str) != Some(app)
    {
        return Err(Error::rejected(
            "catalogue metadata does not match this package",
        ));
    }
    doc.get("releases")
        .and_then(Value::as_array)
        .filter(|releases| releases.len() <= 500)
        .map(Vec::as_slice)
        .ok_or_else(|| Error::rejected("catalogue metadata is unavailable"))
}

fn release_for<'a>(releases: &'a [Value], version: &str) -> Result<&'a Value> {
    let mut matched = releases
        .iter()
        .filter(|release| release.get("version").and_then(Value::as_str) == Some(version));
    let release = matched
        .next()
        .ok_or_else(|| Error::rejected("catalogue metadata does not list this exact release"))?;
    if matched.next().is_some() {
        return Err(Error::rejected(
            "catalogue metadata lists conflicting release identities",
        ));
    }
    Ok(release)
}

fn require_installable(release: &Value, digest: &str) -> Result<()> {
    if release.get("digest").and_then(Value::as_str) != Some(digest)
        || release.get("installable") != Some(&Value::Bool(true))
        || release.get("unavailableReason") != Some(&Value::Null)
    {
        return Err(Error::rejected("catalogue release is not installable"));
    }
    Ok(())
}

/// Discovery is metadata-only. It neither downloads nor replaces a package.
pub(super) fn update_check(
    doc: &Value,
    locator: &CatalogueOrigin,
    package_app: &str,
    installed_version: &str,
    installed_digest: &str,
) -> Result<Value> {
    if locator.version != installed_version || locator.digest != installed_digest {
        return Err(Error::rejected(
            "recorded catalogue origin does not match the installation",
        ));
    }
    let releases = release_list(doc, &locator.id, package_app)?;
    let Some(version) = doc.get("recommended_version").and_then(Value::as_str) else {
        return Ok(
            json!({"schema":1,"source":"catalogue","catalogue_id":locator.id,
            "current_digest":installed_digest,"installed_version":installed_version,
            "has_update":false,"available":false,
            "note":"No eligible published release is available. The installed version is retained."}),
        );
    };
    let release = release_for(releases, version)?;
    let digest = release
        .get("digest")
        .and_then(Value::as_str)
        .filter(|digest| digest_ok(digest))
        .ok_or_else(|| Error::rejected("catalogue release digest is unavailable"))?;
    require_installable(release, digest)?;
    let (Some(installed), Some(candidate)) = (semver(installed_version), semver(version)) else {
        return Err(Error::rejected("catalogue versions are not ordered"));
    };
    if (candidate == installed) != (digest == installed_digest) {
        return Err(Error::rejected(
            "catalogue version and digest disagree with the installation",
        ));
    }
    Ok(
        json!({"schema":1,"source":"catalogue","catalogue_id":locator.id,
        "current_digest":installed_digest,"installed_version":installed_version,
        "version":version,"digest":digest,"has_update":candidate > installed,"available":true}),
    )
}

pub(super) fn authorize(
    directory: CatalogueDirectory<'_>,
    locator: &CatalogueOrigin,
    package_app: &str,
    package_version: &str,
    package_digest: &str,
) -> Result<()> {
    let origin = match directory {
        CatalogueDirectory::Configured => super::client::catalog_origin()
            .ok_or_else(|| Error::rejected("catalogue metadata is unavailable"))?,
        CatalogueDirectory::Origin(origin) => loopback_origin(origin)?,
    };
    let doc = super::client::fetch_versions_at(&origin, &locator.id)?;
    eligible(&doc, locator, package_app, package_version, package_digest)
}

/// Test directories are loopback only. Production uses the closed catalog
/// origin and never a caller-supplied host.
fn loopback_origin(origin: &str) -> Result<String> {
    let origin = origin.trim().trim_end_matches('/');
    let port = origin
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| origin.strip_prefix("http://localhost:"));
    if port.is_none_or(|port| port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit())) {
        return Err(Error::rejected("catalogue metadata is unavailable"));
    }
    Ok(origin.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_locator(version: &str, digest: &str) -> CatalogueOrigin {
        CatalogueOrigin {
            id: "social-content".into(),
            version: version.into(),
            digest: digest.into(),
        }
    }

    fn test_digest(byte: u8) -> String {
        format!("sha256:{byte:02x}{0}", "ab".repeat(31))
    }

    fn versions(version: &str, digest: &str, installable: bool) -> Value {
        json!({
            "app_id": "social-content",
            "app": "social-content",
            "recommended_version": "2.0.0",
            "releases": [{
                "version": version,
                "digest": digest,
                "installable": installable,
                "unavailableReason": if installable { Value::Null } else { json!("disabled") },
                "release_notes": "notes"
            }]
        })
    }

    #[test]
    fn fresh_older_release_is_eligible_and_a_disabled_one_is_not() {
        let digest = test_digest(1);
        let locator = test_locator("1.0.0", &digest);
        eligible(
            &versions("1.0.0", &digest, true),
            &locator,
            "social-content",
            "1.0.0",
            &digest,
        )
        .unwrap();
        let err = eligible(
            &versions("1.0.0", &digest, false),
            &locator,
            "social-content",
            "1.0.0",
            &digest,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not installable"), "{err}");
        let other = test_digest(2);
        let err = eligible(
            &versions("1.0.0", &digest, true),
            &locator,
            "social-content",
            "1.0.0",
            &other,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("does not match this package"),
            "{err}"
        );
    }

    #[test]
    fn metadata_that_cannot_be_read_does_not_authorize() {
        let locator = test_locator("1.0.0", &test_digest(1));
        for doc in [
            json!(null),
            json!({"ok": false}),
            json!({"releases": []}),
            json!({"ok": true, "data": {}}),
        ] {
            let err =
                eligible(&doc, &locator, "social-content", "1.0.0", &test_digest(1)).unwrap_err();
            assert!(
                err.to_string().contains("unavailable")
                    || err.to_string().contains("does not list"),
                "{doc} {err}"
            );
        }
    }

    #[test]
    fn package_identity_and_downgrade_are_refused() {
        let digest = test_digest(4);
        let locator = test_locator("1.0.0", &digest);
        let err = resolve_provenance(
            Some(&test_locator("2.0.0", &test_digest(3))),
            &CatalogueClaim::Locator(locator.clone()),
            true,
            Some("2.0.0"),
            "1.0.0",
            &digest,
        )
        .unwrap_err();
        assert!(err.to_string().contains("older release"), "{err}");
        let other = CatalogueOrigin {
            id: "other-app".into(),
            ..locator.clone()
        };
        let err = resolve_provenance(
            Some(&locator),
            &CatalogueClaim::Locator(other),
            true,
            Some("1.0.0"),
            "1.0.0",
            &digest,
        )
        .unwrap_err();
        assert!(err.to_string().contains("catalogue id"), "{err}");
        let err = resolve_provenance(
            Some(&locator),
            &CatalogueClaim::Absent,
            true,
            Some("1.0.0"),
            "1.0.0",
            &digest,
        )
        .unwrap_err();
        assert!(err.to_string().contains("names the new release"), "{err}");
    }

    #[test]
    fn omission_does_not_invent_origin_and_manual_clears_it() {
        let fresh = resolve_provenance(
            None,
            &CatalogueClaim::Absent,
            false,
            None,
            "1.0.0",
            &test_digest(1),
        )
        .unwrap();
        assert!(fresh.origin.is_none() && !fresh.manual);
        let cleared = resolve_provenance(
            Some(&test_locator("1.0.0", &test_digest(1))),
            &CatalogueClaim::Manual,
            true,
            Some("1.0.0"),
            "1.1.0",
            &test_digest(2),
        )
        .unwrap();
        assert!(cleared.origin.is_none() && cleared.manual);
    }

    #[test]
    fn catalogue_incomplete_or_ambiguous_eligibility_does_not_authorize() {
        let digest = test_digest(1);
        let locator = test_locator("1.0.0", &digest);
        let mut missing_reason = versions("1.0.0", &digest, true);
        missing_reason["releases"][0]
            .as_object_mut()
            .unwrap()
            .remove("unavailableReason");
        let mut ambiguous = versions("1.0.0", &digest, true);
        let mut conflicting = ambiguous["releases"][0].clone();
        conflicting["digest"] = json!(test_digest(2));
        ambiguous["releases"]
            .as_array_mut()
            .unwrap()
            .push(conflicting);
        for document in [missing_reason, ambiguous] {
            let error =
                eligible(&document, &locator, "social-content", "1.0.0", &digest).unwrap_err();
            assert_eq!(error.kind(), "rejected");
        }
    }

    #[test]
    fn catalogue_update_discovery_keeps_newer_installs_and_reports_withdrawal() {
        let old_digest = test_digest(1);
        let installed_digest = test_digest(2);
        let installed = test_locator("2.0.0", &installed_digest);
        let mut newer_document = versions("3.0.0", &test_digest(3), true);
        newer_document["recommended_version"] = json!("3.0.0");
        let newer = update_check(
            &newer_document,
            &installed,
            "social-content",
            "2.0.0",
            &installed_digest,
        )
        .unwrap();
        assert_eq!(newer["has_update"], true);
        assert_eq!(newer["version"], "3.0.0");
        let mut document = versions("1.0.0", &old_digest, true);
        document["recommended_version"] = json!("1.0.0");
        let older = update_check(
            &document,
            &installed,
            "social-content",
            "2.0.0",
            &installed_digest,
        )
        .unwrap();
        assert_eq!(older["has_update"], false);
        assert_eq!(older["installed_version"], "2.0.0");
        assert_eq!(older["current_digest"], installed_digest);

        document["recommended_version"] = Value::Null;
        let withdrawn = update_check(
            &document,
            &installed,
            "social-content",
            "2.0.0",
            &installed_digest,
        )
        .unwrap();
        assert_eq!(withdrawn["available"], false);
        assert_eq!(withdrawn["has_update"], false);
        assert_eq!(withdrawn["current_digest"], installed_digest);

        document = versions("2.0.0", &old_digest, true);
        assert_eq!(
            update_check(
                &document,
                &installed,
                "social-content",
                "2.0.0",
                &installed_digest
            )
            .unwrap_err()
            .kind(),
            "rejected"
        );
    }
}
