// Bounded test-only runtime TLS adapter candidate, cad1085-security.
// No private key or trust-bypass literal in tracked source. No sealed drivers edited.
#[cfg(not(all(test, unix, feature = "test-seam")))]
compile_error!("ephemeral custody TLS is a test-only fixture");

use crate::error::{Error, Result};
use std::io::{BufReader, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

struct Material {
    ca_pem: Vec<u8>,
    server: Arc<rustls::ServerConfig>,
}
static MATERIAL: LazyLock<Result<Material>> = LazyLock::new(generate);

fn refused(stage: &'static str) -> Error {
    Error::internal(format!("ephemeral fixture TLS {stage} failed"))
}
fn command(dir: &Path, arguments: &[&str], stage: &'static str) -> Result<()> {
    // Actual installed OpenSSL, file paths only. Native identity untouched.
    let mut child = Command::new("/usr/bin/openssl")
        .args(arguments)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| refused(stage))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    // Only fixed stage and numeric status; no tool text/keys.
                    eprintln!(
                        "ephemeral fixture TLS stage={stage} exit={:?}",
                        status.code()
                    );
                    return Err(refused(stage));
                }
                return Ok(());
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                // Only the exact generator child this function spawned.
                let _ = child.kill();
                let _ = child.wait();
                return Err(refused(stage));
            }
        }
    }
}
fn owner_file(dir: &Path, name: &str, content: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join(name))
        .map_err(|_| refused("exclusive-file"))?;
    file.write_all(content)
        .map_err(|_| refused("exclusive-file"))?;
    file.sync_all().map_err(|_| refused("exclusive-file"))?;
    Ok(())
}
fn generate() -> Result<Material> {
    let root = tempfile::Builder::new()
        .prefix("c1128tls-")
        .tempdir_in(std::env::temp_dir())
        .map_err(|_| refused("temp-root"))?;
    // Fresh owned fixture root only, never chmod an existing user directory.
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .map_err(|_| refused("temp-root"))?;
    let dir = root.path();
    for name in [
        "ca-key.pem",
        "leaf-key.pem",
        "ca.pem",
        "leaf.pem",
        "leaf.csr",
    ] {
        owner_file(dir, name, b"")?;
    }
    owner_file(dir, "leaf.ext", b"basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n")?;
    command(
        dir,
        &[
            "genpkey",
            "-algorithm",
            "EC",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-out",
            "ca-key.pem",
        ],
        "ca-key",
    )?;
    command(
        dir,
        &[
            "req",
            "-new",
            "-x509",
            "-key",
            "ca-key.pem",
            "-out",
            "ca.pem",
            "-days",
            "2",
            "-sha256",
            "-subj",
            "/CN=CAD-1128 ephemeral fixture CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ],
        "ca-certificate",
    )?;
    command(
        dir,
        &[
            "genpkey",
            "-algorithm",
            "EC",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-out",
            "leaf-key.pem",
        ],
        "leaf-key",
    )?;
    command(
        dir,
        &[
            "req",
            "-new",
            "-key",
            "leaf-key.pem",
            "-out",
            "leaf.csr",
            "-subj",
            "/CN=localhost",
        ],
        "leaf-request",
    )?;
    command(
        dir,
        &[
            "x509",
            "-req",
            "-in",
            "leaf.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca-key.pem",
            "-set_serial",
            "2",
            "-out",
            "leaf.pem",
            "-days",
            "2",
            "-sha256",
            "-extfile",
            "leaf.ext",
        ],
        "leaf-certificate",
    )?;
    command(
        dir,
        &[
            "verify",
            "-CAfile",
            "ca.pem",
            "-purpose",
            "sslserver",
            "-verify_hostname",
            "localhost",
            "leaf.pem",
        ],
        "verify-hostname",
    )?;
    command(
        dir,
        &[
            "verify",
            "-CAfile",
            "ca.pem",
            "-purpose",
            "sslserver",
            "-verify_ip",
            "127.0.0.1",
            "leaf.pem",
        ],
        "verify-loopback",
    )?;
    for name in ["ca-key.pem", "leaf-key.pem"] {
        let metadata =
            std::fs::symlink_metadata(dir.join(name)).map_err(|_| refused("key-ownership"))?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(refused("key-ownership"));
        }
    }
    let ca_pem = std::fs::read(dir.join("ca.pem")).map_err(|_| refused("ca-read"))?;
    let leaf_pem = std::fs::read(dir.join("leaf.pem")).map_err(|_| refused("leaf-read"))?;
    let key_pem = zeroize::Zeroizing::new(
        std::fs::read(dir.join("leaf-key.pem")).map_err(|_| refused("key-read"))?,
    );
    let certs = rustls_pemfile::certs(&mut BufReader::new(leaf_pem.as_slice()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| refused("leaf-parse"))?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_slice()))
        .map_err(|_| refused("key-parse"))?
        .ok_or_else(|| refused("key-parse"))?;
    let server = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|_| refused("server-config"))?,
    );
    // Drop no longer used CA signing material and all named fixture files.
    // TempDir also owns cleanup on every early error; no shared path removal.
    root.close().map_err(|_| refused("owned-cleanup"))?;
    Ok(Material { ca_pem, server })
}
pub(super) fn ca_pem() -> Result<Vec<u8>> {
    MATERIAL
        .as_ref()
        .map(|material| material.ca_pem.clone())
        .map_err(|_| refused("material"))
}
pub(super) fn server_config() -> Result<Arc<rustls::ServerConfig>> {
    MATERIAL
        .as_ref()
        .map(|material| material.server.clone())
        .map_err(|_| refused("material"))
}
