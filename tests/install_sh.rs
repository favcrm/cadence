//! CAD-311: `install.sh` against a fake release served from `file://`,
//! in a temp HOME and prefix under a short `/tmp` root. Nothing here
//! downloads from GitHub or touches the real `~/.local`.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use cadence_agent::upgrade::Layout;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// The asset suffix `install.sh` picks on this machine.
fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-linux",
        ("linux", "aarch64") => "aarch64-linux",
        ("macos", "aarch64") => "aarch64-macos",
        other => panic!("no release target for {other:?}"),
    }
}

fn asset() -> String {
    format!("cadence-{}-{}.tar.gz", tag(), target())
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh")
}

/// A temp root, a HOME inside it, and a release source at `dl/<tag>/`.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c311-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "tmp", "dl", "api/releases"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let fx = Self { root };
        fx.publish(SHA, None, false);
        fx
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    fn link(&self) -> PathBuf {
        self.home().join(".local/bin/cadence")
    }

    fn default_releases(&self) -> PathBuf {
        self.home().join(".local/share/cadence/releases")
    }

    /// Write the tarball and its `.sha256` as the release workflow does:
    /// `cadence`, `cadence.sha256`, `manifest.json` at the tarball root.
    /// `published` overrides the outer checksum; `bad_inner` corrupts the
    /// binary's own checksum inside the tarball.
    fn publish(&self, sha: &str, published: Option<&str>, bad_inner: bool) {
        let stage = self.path("stage");
        let _ = fs::remove_dir_all(&stage);
        fs::create_dir_all(&stage).unwrap();
        let bin = format!(
            "#!/bin/sh\necho \"cadence {}+{sha}\"\n",
            env!("CARGO_PKG_VERSION")
        );
        fs::write(stage.join("cadence"), &bin).unwrap();
        fs::set_permissions(stage.join("cadence"), fs::Permissions::from_mode(0o755)).unwrap();
        let inner = if bad_inner {
            "0".repeat(64)
        } else {
            hex(bin.as_bytes())
        };
        fs::write(stage.join("cadence.sha256"), format!("{inner}  cadence\n")).unwrap();
        fs::write(
            stage.join("manifest.json"),
            serde_json::json!({
                "source_sha": sha, "version": tag(), "target": target(),
                "features": ["ui"], "sha256": hex(bin.as_bytes()),
            })
            .to_string(),
        )
        .unwrap();
        let out_dir = self.path("dl").join(tag());
        fs::create_dir_all(&out_dir).unwrap();
        let tarball = out_dir.join(asset());
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&stage)
            .args(["cadence", "cadence.sha256", "manifest.json"])
            .status()
            .unwrap();
        assert!(status.success());
        let digest = published
            .map(str::to_string)
            .unwrap_or_else(|| hex(&fs::read(&tarball).unwrap()));
        fs::write(
            out_dir.join(format!("{}.sha256", asset())),
            format!("{digest}  {}\n", asset()),
        )
        .unwrap();
    }

    fn install(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new("sh");
        cmd.arg(script()).args(args);
        self.env(&mut cmd).output().unwrap()
    }

    /// `curl … | sh`: the script arrives on stdin.
    fn install_piped(&self, script: &[u8]) -> Output {
        let mut cmd = Command::new("sh");
        let mut child = self
            .env(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(script).unwrap();
        child.wait_with_output().unwrap()
    }

    fn env<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        cmd.env("HOME", self.home())
            .env("TMPDIR", self.path("tmp"))
            .env_remove("XDG_DATA_HOME")
            .env(
                "CADENCE_INSTALL_BASE_URL",
                format!("file://{}", self.path("dl").display()),
            )
            .env(
                "CADENCE_INSTALL_API_URL",
                format!("file://{}", self.path("api").display()),
            )
    }

    fn install_ok(&self, args: &[&str]) -> String {
        let out = self.install(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "install.sh failed:\n{text}");
        text
    }

    fn install_err(&self, args: &[&str]) -> String {
        let out = self.install(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "install.sh should refuse:\n{text}");
        text
    }

    /// Names in `dir`, sorted — for catching leftover temp files.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

#[test]
fn installs_a_verified_release_under_the_default_prefix_and_links_it() {
    let fx = Fixture::new();
    let text = fx.install_ok(&["--version", &tag()]);
    assert!(text.contains("checksum ok"), "{text}");
    let dest = fx.default_releases().join(tag());
    for f in ["cadence", "cadence.sha256", "manifest.json"] {
        assert!(dest.join(f).is_file(), "missing {f}");
    }
    let mode = fs::metadata(dest.join("cadence"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(fs::read_link(fx.link()).unwrap(), dest.join("cadence"));
    assert!(text.contains(&format!("+{SHA}")), "{text}");
    assert_eq!(Fixture::entries(&fx.default_releases()), vec![tag()]);
    assert_eq!(Fixture::entries(&fx.path("tmp")), Vec::<String>::new());
    // `cadence upgrade` reads the same releases dir off the link.
    let layout = Layout::detect(Some(fx.link()), None).unwrap();
    assert_eq!(layout.releases, fx.default_releases());
}

#[test]
fn a_rerun_is_idempotent() {
    let fx = Fixture::new();
    fx.install_ok(&["--version", &tag()]);
    let bin = fx.default_releases().join(tag()).join("cadence");
    let before = fs::metadata(&bin).unwrap();
    // The bare version form names the same tag.
    let text = fx.install_ok(&["--version", env!("CARGO_PKG_VERSION")]);
    assert!(text.contains("already installed"), "{text}");
    assert!(text.contains("link unchanged"), "{text}");
    let after = fs::metadata(&bin).unwrap();
    assert_eq!((before.ino(), before.mtime()), (after.ino(), after.mtime()));
    assert_eq!(fs::read_link(fx.link()).unwrap(), bin);
    assert_eq!(Fixture::entries(&fx.default_releases()), vec![tag()]);
    assert_eq!(
        Fixture::entries(&fx.home().join(".local/bin")),
        vec!["cadence"]
    );
}

#[test]
fn a_checksum_mismatch_is_refused_and_nothing_is_installed() {
    let fx = Fixture::new();
    fx.publish(SHA, Some(&"1".repeat(64)), false);
    let text = fx.install_err(&["--version", &tag()]);
    assert!(text.contains("checksum mismatch"), "{text}");
    assert!(!fx.default_releases().join(tag()).exists());
    assert!(fs::symlink_metadata(fx.link()).is_err());
    assert_eq!(Fixture::entries(&fx.path("tmp")), Vec::<String>::new());
}

#[test]
fn a_binary_that_does_not_match_its_inner_checksum_is_refused() {
    let fx = Fixture::new();
    fx.publish(SHA, None, true);
    let text = fx.install_err(&["--version", &tag()]);
    assert!(text.contains("does not match its cadence.sha256"), "{text}");
    assert!(!fx.default_releases().join(tag()).exists());
    assert!(fs::symlink_metadata(fx.link()).is_err());
}

#[test]
fn prefix_is_honoured_in_both_spellings() {
    let fx = Fixture::new();
    let prefix = fx.path("p");
    let prefix_arg = prefix.to_string_lossy().into_owned();
    fx.install_ok(&["--prefix", &prefix_arg, "--version", &tag()]);
    let bin = prefix.join("releases").join(tag()).join("cadence");
    assert!(bin.is_file());
    assert_eq!(fs::read_link(fx.link()).unwrap(), bin);
    assert!(!fx.default_releases().exists());
    let layout = Layout::detect(Some(fx.link()), None).unwrap();
    assert_eq!(layout.releases, prefix.join("releases"));

    let other = fx.path("q");
    fx.install_ok(&[
        &format!("--prefix={}", other.display()),
        &format!("--version={}", tag()),
    ]);
    let bin = other.join("releases").join(tag()).join("cadence");
    assert_eq!(fs::read_link(fx.link()).unwrap(), bin);
}

#[test]
fn latest_resolves_the_tag_from_the_release_api() {
    let fx = Fixture::new();
    fs::write(
        fx.path("api/releases/latest"),
        format!(
            "{{\n  \"url\": \"x\",\n  \"tag_name\": \"{}\",\n  \"name\": \"y\"\n}}\n",
            tag()
        ),
    )
    .unwrap();
    fx.install_ok(&[]);
    assert!(fx.default_releases().join(tag()).join("cadence").is_file());
}

#[test]
fn a_regular_file_at_the_link_is_never_replaced() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.home().join(".local/bin")).unwrap();
    fs::write(fx.link(), b"mine").unwrap();
    let text = fx.install_err(&["--version", &tag()]);
    assert!(text.contains("not a symlink"), "{text}");
    assert_eq!(fs::read(fx.link()).unwrap(), b"mine");
}

#[test]
fn a_release_dir_with_other_bytes_is_never_overwritten() {
    let fx = Fixture::new();
    let dest = fx.default_releases().join(tag());
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("cadence"), b"something else").unwrap();
    let text = fx.install_err(&["--version", &tag()]);
    assert!(
        text.contains("does not hold this release's binary"),
        "{text}"
    );
    assert_eq!(fs::read(dest.join("cadence")).unwrap(), b"something else");
    assert!(fs::symlink_metadata(fx.link()).is_err());
}

#[test]
fn a_tag_that_is_not_plain_is_refused_before_any_download() {
    let fx = Fixture::new();
    for bad in ["v1/../../x", "v", "v1 2"] {
        let text = fx.install_err(&["--version", bad]);
        assert!(text.contains("not a release tag"), "{bad}: {text}");
    }
    assert!(!fx.home().join(".local").exists());
}

#[test]
fn upgrade_reads_a_releases_dir_only_off_a_version_or_sha_dir() {
    let fx = Fixture::new();
    let link = fx.path("link");
    for (dir, found) in [
        ("r/v0.1.0", true),
        ("r/v2.3.4-rc.1", true),
        (&format!("r/{SHA}")[..], true),
        ("r/latest", false),
        ("r/vx", false),
    ] {
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(fx.path(dir).join("cadence"), &link).unwrap();
        let layout = Layout::detect(Some(link.clone()), None).unwrap();
        assert_eq!(layout.releases == fx.path("r"), found, "{dir}");
    }
}

#[test]
fn a_redirected_source_is_named_in_the_output() {
    let fx = Fixture::new();
    let text = fx.install_ok(&["--version", &tag()]);
    assert!(text.contains("source: file://"), "{text}");
    assert!(text.contains("not the default"), "{text}");
}

/// A `curl | sh` cut short must never install, link or say `ok`: every
/// statement runs inside `main`, which is called on the last line and
/// refuses to run without the `--end-of-script` marker that ends it.
#[test]
fn a_truncated_script_piped_into_sh_installs_nothing() {
    let publish_latest = |fx: &Fixture| {
        fs::write(
            fx.path("api/releases/latest"),
            format!("{{\"tag_name\": \"{}\"}}\n", tag()),
        )
        .unwrap();
    };
    let full = fs::read(script()).unwrap();
    // Whole, the piped script installs and links — so every cut below
    // is refused by the script's shape, not by a missing fixture.
    let whole = Fixture::new();
    publish_latest(&whole);
    let out = whole.install_piped(&full);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fs::symlink_metadata(whole.link()).is_ok());

    let call = full.len() - b"main \"$@\" --end-of-script\n".len();
    let mut cuts: Vec<usize> = (1..20).map(|i| full.len() * i / 20).collect();
    // Around the call: before it, `main` alone, `main "$@"`, the marker
    // half-sent, and all but the final newline (which still runs).
    cuts.extend([
        call - 1,
        call,
        call + 4,
        call + 9,
        call + 15,
        full.len() - 2,
    ]);
    for cut in cuts {
        let fx = Fixture::new();
        publish_latest(&fx);
        let out = fx.install_piped(&full[..cut]);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!text.contains("cadence-install: ok"), "cut {cut}: {text}");
        assert!(
            !out.status.success() || text.is_empty(),
            "cut {cut} exited 0 after doing something: {text}"
        );
        assert!(fs::symlink_metadata(fx.link()).is_err(), "cut {cut} linked");
        assert!(
            !fx.home().join(".local").exists(),
            "cut {cut} wrote ~/.local"
        );
    }
}
