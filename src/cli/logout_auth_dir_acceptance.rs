use super::*;
use std::path::PathBuf;

#[test]
fn remote_logout_accepts_and_preserves_org_with_custom_auth_dir() {
    let parsed = Cli::try_parse_from([
        "cadence",
        "auth",
        "logout",
        "--org",
        "acme",
        "--auth-dir",
        "/tmp/cad1339-auth",
    ])
    .unwrap();
    match parsed.command {
        Commands::Auth {
            action: AuthAction::Logout { org, auth_dir },
        } => {
            assert_eq!(org.as_deref(), Some("acme"));
            assert_eq!(auth_dir, Some(PathBuf::from("/tmp/cad1339-auth")));
        }
        _ => panic!("unexpected command/action"),
    }
}

#[test]
fn legacy_logout_without_org_still_accepts_custom_auth_dir() {
    let parsed = Cli::try_parse_from([
        "cadence",
        "auth",
        "logout",
        "--auth-dir",
        "/tmp/cad1339-auth",
    ])
    .unwrap();
    match parsed.command {
        Commands::Auth {
            action: AuthAction::Logout { org, auth_dir },
        } => {
            assert_eq!(org, None);
            assert_eq!(auth_dir, Some(PathBuf::from("/tmp/cad1339-auth")));
        }
        _ => panic!("unexpected command/action"),
    }
}
