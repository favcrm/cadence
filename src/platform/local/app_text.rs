//! Internal text sink. The broker holds the release mutex through this commit;
//! no SQL lock survives the checked claim or the provider's read-back.
use super::{sha256_hex, LocalAdapter, BODY_CAP, TITLE_CAP};
use crate::contract_fixture::Verified;
use crate::platform::PlatformAdapter;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

const INPUT_CAP: usize = 64 * 1024;
const APP_BUCKET: &str = "app-items";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AppTextInput {
    schema: u32,
    title: String,
    body: String,
    provenance: Value,
}

fn parse(input: &Value) -> Result<AppTextInput, String> {
    if input.to_string().len() > INPUT_CAP {
        return Err("app text input exceeds its encoded bound".into());
    }
    let text: AppTextInput =
        serde_json::from_value(input.clone()).map_err(|_| "invalid app text input".to_string())?;
    if text.schema != 1
        || text.title.trim().is_empty()
        || text.title.chars().count() > TITLE_CAP
        || text.title.chars().any(char::is_control)
        || text.body.is_empty()
        || text.body.len() > BODY_CAP
        || text.body.contains('\0')
        || text
            .provenance
            .as_object()
            .is_none_or(|object| object.is_empty())
    {
        return Err("invalid app text material or provenance".into());
    }
    Ok(text)
}

fn render(text: &AppTextInput) -> String {
    format!("# {}\n\n{}", text.title, text.body)
}

pub(super) fn prepare(title: &str, body: &str, provenance: &Value) -> Result<Value, String> {
    let input = json!({"schema":1,"title":title,"body":body,"provenance":provenance});
    parse(&input)?;
    Ok(input)
}

pub(super) fn preview(account: &str, input: &Value) -> String {
    match parse(input) {
        Ok(text) => format!(
            "Release reviewed app artifact to Local (account {account}):\n\n{}\n\nProvenance: {}",
            render(&text),
            text.provenance
        ),
        Err(_) => "Local app artifact input is invalid; release will refuse".into(),
    }
}

fn name(value: &[u8]) -> Result<std::ffi::CString, String> {
    std::ffi::CString::new(value).map_err(|_| "invalid outbox component".into())
}

fn child(parent: &File, component: &[u8], create: bool) -> Result<File, String> {
    let component = name(component)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut fd = unsafe { libc::openat(parent.as_raw_fd(), component.as_ptr(), flags) };
    if fd < 0 && create && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) };
        if result != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err("cannot create private app outbox directory".into());
        }
        fd = unsafe { libc::openat(parent.as_raw_fd(), component.as_ptr(), flags) };
    }
    if fd < 0 {
        return Err("app outbox directory is unavailable or symlinked".into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn bucket(path: &Path, create: bool) -> Result<File, String> {
    let mut directory = File::open(if path.is_absolute() { "/" } else { "." })
        .map_err(|_| "cannot open outbox root".to_string())?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(component) => {
                directory = child(&directory, component.as_bytes(), create)?
            }
            _ => return Err("app outbox path contains an unsupported component".into()),
        }
    }
    let metadata = directory
        .metadata()
        .map_err(|_| "cannot inspect app outbox".to_string())?;
    if metadata.mode() & 0o022 != 0 {
        return Err("app outbox is writable by other principals".into());
    }
    let directory = child(&directory, APP_BUCKET.as_bytes(), create)?;
    if directory
        .metadata()
        .map_err(|_| "cannot inspect app bucket".to_string())?
        .mode()
        & 0o022
        != 0
    {
        return Err("app outbox bucket is writable by other principals".into());
    }
    Ok(directory)
}

fn read_file(parent: &File, component: &str) -> Result<Vec<u8>, String> {
    let component = name(component.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err("app outbox record is unavailable".into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file
        .metadata()
        .map_err(|_| "cannot inspect app record".to_string())?
        .is_file()
    {
        return Err("app outbox record is not regular".into());
    }
    let mut bytes = Vec::new();
    file.take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read app record".to_string())?;
    if bytes.len() > 128 * 1024 {
        return Err("app outbox record exceeds its bound".into());
    }
    Ok(bytes)
}

fn write_file(parent: &File, component: &str, bytes: &[u8]) -> Result<(), String> {
    let component = name(component.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err("cannot create app outbox record".into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "cannot commit app outbox record".into())
}

pub(super) fn execute(
    adapter: &LocalAdapter,
    input: &Value,
    effect_id: &str,
    expected_hash: Option<&str>,
) -> Result<Value, String> {
    let text = parse(input)?;
    crate::proto::identifier(effect_id, "effect_id")
        .map_err(|_| "invalid app effect id".to_string())?;
    let permit = crate::store::app_effects::read_execution_permit(
        &adapter.state_dir.join("cadence.sqlite3"),
        effect_id,
    )
    .map_err(|_| "persisted app artifact execution permit is unavailable".to_string())?;
    let input_digest = format!("sha256:{}", sha256_hex(input.to_string().as_bytes()));
    let artifact_digest = format!("sha256:{}", sha256_hex(text.body.as_bytes()));
    if permit.effect_id != effect_id
        || permit.platform != "local"
        || permit.account != "local"
        || permit.tool != "publish_app_text"
        || text.provenance["effect_id"] != effect_id
        || text.provenance["authorization_kind"] != "app_artifact"
        || text.provenance["authority_digest"] != permit.authority_digest
        || permit.input != *input
        || permit.input_digest != input_digest
        || permit.provenance != text.provenance
        || permit.authority_digest.is_empty()
        || expected_hash != Some(artifact_digest.as_str())
        || text.provenance["artifact_digest"] != artifact_digest
        || text.provenance["sink_registration"].as_str()
            != adapter.connection_registration().as_deref()
    {
        return Err("app artifact input, authority or sink changed before commit".into());
    }
    let parent = bucket(&adapter.outbox, true)?;
    if let Ok(item) = child(&parent, effect_id.as_bytes(), false) {
        let index: Value = serde_json::from_slice(&read_file(&item, "index.json")?)
            .map_err(|_| "invalid app outbox index".to_string())?;
        if index["effect_id"] != effect_id
            || index["scope"]["kind"] != "app_artifact"
            || index["provenance"] != text.provenance
            || index["input_digest"] != input_digest
            || index["authority_digest"] != permit.authority_digest
            || read_file(&item, "post.md")? != render(&text).as_bytes()
        {
            return Err("app outbox replay differs from recorded material".into());
        }
        return Ok(index["result"].clone());
    }
    let temporary = format!(".tmp-{}", uuid::Uuid::new_v4());
    let item = child(&parent, temporary.as_bytes(), true)?;
    let post = render(&text);
    let board_url = format!("{}/outbox?item={effect_id}", adapter.board_url());
    let result = json!({"schema":1,"scope":{"kind":"app_artifact","install_id":text.provenance["install_id"],"context_id":text.provenance["context_id"]},
        "platform_ref":format!("outbox/{APP_BUCKET}/{effect_id}"),"board_url":board_url,"url":board_url,
        "path":adapter.outbox.join(APP_BUCKET).join(effect_id).display().to_string(),"attachments":0});
    let index = json!({"schema":1,"effect_id":effect_id,"scope":result["scope"],"project":null,"title":text.title,
        "provenance":text.provenance,"authority_digest":permit.authority_digest,"input_digest":input_digest,
        "input_sha256":sha256_hex(input.to_string().as_bytes()),"post_sha256":sha256_hex(post.as_bytes()),
        "content_sha256":sha256_hex(post.as_bytes()),"published_at":crate::issue::time::iso(crate::issue::time::now_epoch()),
        "attachments":[],"result":result});
    let mut committed = false;
    let landed = (|| {
        write_file(&item, "post.md", post.as_bytes())?;
        write_file(&item, "index.json", index.to_string().as_bytes())?;
        item.sync_all()
            .map_err(|_| "cannot sync app item".to_string())?;
        let source = name(temporary.as_bytes())?;
        let destination = name(effect_id.as_bytes())?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                source.as_ptr(),
                parent.as_raw_fd(),
                destination.as_ptr(),
            )
        } != 0
        {
            return Err("cannot atomically land app outbox item".into());
        }
        committed = true;
        parent
            .sync_all()
            .map_err(|_| "cannot sync app outbox".to_string())
    })();
    if let Err(error) = landed {
        // A sync failure after rename is uncertain, not permission to destroy
        // the already-complete item. The broker retains that uncertainty.
        if committed {
            return Err(error);
        }
        for component in ["post.md", "index.json"] {
            let component = name(component.as_bytes())?;
            unsafe {
                libc::unlinkat(item.as_raw_fd(), component.as_ptr(), 0);
            }
        }
        let temporary = name(temporary.as_bytes())?;
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), libc::AT_REMOVEDIR);
        }
        return Err(error);
    }
    Ok(result)
}

pub(super) fn read_back(adapter: &LocalAdapter, input: &Value) -> Verified {
    let Ok(text) = parse(input) else {
        return Verified::False;
    };
    let Some(effect_id) = text.provenance["effect_id"].as_str() else {
        return Verified::False;
    };
    let Some(authority_digest) = text.provenance["authority_digest"]
        .as_str()
        .filter(|digest| !digest.is_empty())
    else {
        return Verified::False;
    };
    if crate::proto::identifier(effect_id, "effect_id").is_err()
        || text.provenance["authorization_kind"] != "app_artifact"
        || text.provenance["sink_registration"].as_str()
            != adapter.connection_registration().as_deref()
    {
        return Verified::False;
    }
    let digest = format!("sha256:{}", sha256_hex(input.to_string().as_bytes()));
    let Ok(parent) = bucket(&adapter.outbox, false) else {
        return Verified::Unknown;
    };
    let Ok(item) = child(&parent, effect_id.as_bytes(), false) else {
        return Verified::Unknown;
    };
    let Ok(bytes) = read_file(&item, "index.json") else {
        return Verified::False;
    };
    let Ok(index) = serde_json::from_slice::<Value>(&bytes) else {
        return Verified::False;
    };
    if index["effect_id"] != effect_id
        || index["authority_digest"] != authority_digest
        || index["input_digest"] != digest
        || index["scope"]["kind"] != "app_artifact"
        || index["provenance"] != text.provenance
    {
        return Verified::False;
    }
    if read_file(&item, "post.md").is_ok_and(|bytes| bytes == render(&text).as_bytes()) {
        Verified::True
    } else {
        Verified::False
    }
}
