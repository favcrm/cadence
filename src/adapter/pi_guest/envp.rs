//! The post-drop guest environment — a fixed, enumerated `KEY=VAL` vector the
//! helper assembles *after* the capability seal and uid drop. No caller
//! `--env` supplies `HOME`, a loader, or a credential name; no broad
//! `CADENCE_*`/`PI_*`/`XDG_*` prefix is forwarded. `CADENCE_STATE_DIR` (the
//! private producer state dir) is never present.

use std::ffi::CString;

use super::GuestCtx;

/// The exact guest envp set — order-fixed, every value derived from the
/// verified per-launch view. `child_env`'s base (`PATH`/`USER`/`LOGNAME`/
/// `SHELL`) is supplied by the helper from the guest passwd entry; this list
/// is appended post-drop.
pub(crate) fn guest_envp(ctx: &GuestCtx) -> Vec<CString> {
    // The canonical view paths come from the topology constants — the guest
    // never supplies a path fragment.
    let gen = ctx.segments().generation_hex();
    let view = format!(
        "/srv/cadence/guest-views/{}/{}",
        ctx.segments().alias_hex(),
        gen
    );
    let mut out: Vec<CString> = Vec::new();
    let mut push = |kv: String| {
        // No interior NUL can be present — every component is a fixed token.
        out.push(CString::new(kv).expect("guest env carries no NUL"));
    };
    push(format!("HOME={view}/home"));
    push(format!("PI_CODING_AGENT_DIR={view}/config"));
    push(format!("XDG_CACHE_HOME={view}/cache"));
    push(format!("TMPDIR={view}/tmp"));
    push(format!("GH_CONFIG_DIR={view}/no-forge"));
    push(format!("GIT_CONFIG_GLOBAL={view}/config/gitconfig"));
    push("GIT_TERMINAL_PROMPT=0".to_string());
    push("PI_OFFLINE=1".to_string());
    push("CADENCE_SOCKET=/var/lib/cadence/cadence.sock".to_string());
    push(format!("CADENCE_ALIAS={}", ctx.alias()));
    push("CADENCE_PM_DIR=/workspace/company/pm".to_string());
    out
}

/// The exact variable names the guest envp may carry — a fixed allowlist the
/// test asserts is complete and non-overlapping with forbidden prefixes.
pub(crate) const GUEST_ENV_NAMES: &[&str] = &[
    "HOME",
    "PI_CODING_AGENT_DIR",
    "XDG_CACHE_HOME",
    "TMPDIR",
    "GH_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_TERMINAL_PROMPT",
    "PI_OFFLINE",
    "CADENCE_SOCKET",
    "CADENCE_ALIAS",
    "CADENCE_PM_DIR",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The env set is exactly the finite allowlist — and it can never contain
    /// a credential, a loader variable, or the private state dir.
    #[test]
    fn guest_envp_names_are_the_finite_set() {
        assert_eq!(GUEST_ENV_NAMES.len(), 11);
        for name in GUEST_ENV_NAMES {
            // never a private-authority name, loader, or credential shape
            assert_ne!(*name, "CADENCE_STATE_DIR");
            assert!(!name.starts_with("LD_"));
            assert!(!name.starts_with("NODE_"));
            assert!(!name.ends_with("_KEY") && !name.ends_with("_TOKEN"));
        }
        // the exact set, no extras
        let mut sorted = GUEST_ENV_NAMES.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![
                "CADENCE_ALIAS",
                "CADENCE_PM_DIR",
                "CADENCE_SOCKET",
                "GH_CONFIG_DIR",
                "GIT_CONFIG_GLOBAL",
                "GIT_TERMINAL_PROMPT",
                "HOME",
                "PI_CODING_AGENT_DIR",
                "PI_OFFLINE",
                "TMPDIR",
                "XDG_CACHE_HOME",
            ]
        );
    }
}
