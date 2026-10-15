//! CAD-1393: `cadence template` and `cadence document` — company document
//! templates and drafts over the daemon's business-template RPCs. Input is
//! a JSON file; the daemon validates, calculates and enforces scope.
//!
//! Quotation and invoice versions keep a DOCX source. An authoring pack
//! (`kind` is `authoring`) is guidelines and reference bytes only: create
//! and revise omit `--docx`, and `show --out` writes that exact version
//! into a new directory. It does not render, price, or approve anything.

use super::*;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Subcommand)]
pub(crate) enum TemplateAction {
    /// Create version 1 from a spec JSON file. A quotation or invoice also
    /// needs `--docx`. An authoring pack must omit `--docx`. The daemon
    /// assigns the version; the result is a draft and is not approved.
    Create {
        /// The template name (lowercase letters, digits, '-').
        name: String,
        /// DOCX source for a quotation or invoice. Refused for an authoring pack.
        #[arg(long)]
        docx: Option<PathBuf>,
        /// Spec JSON. Financial: kind, fields, fixed_text, calculation.
        /// Authoring: kind "authoring", purpose, format, guidelines, references.
        #[arg(long)]
        file: PathBuf,
    },
    /// Create the next version from `--base`. The base is left unchanged;
    /// the result is a new draft and does not inherit approval.
    Revise {
        name: String,
        /// The version this revision starts from.
        #[arg(long)]
        base: i64,
        /// DOCX source for a quotation or invoice. Refused for an authoring pack.
        #[arg(long)]
        docx: Option<PathBuf>,
        #[arg(long)]
        file: PathBuf,
    },
    /// List the versions of a template, or show one with `--version`.
    /// `--out` materialises one authoring pack into a new directory.
    Show {
        name: String,
        #[arg(long)]
        version: Option<i64>,
        /// New directory for an authoring pack (`guidelines.md`, reference
        /// files, `template.json`). Requires `--version`. Must not exist.
        /// Created mode 0700; files inside are created 0644. Refused for a
        /// quotation or invoice.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Read a company DOCX or readable PDF as a reference: its text,
    /// placeholders, loop tags, tables and styles, with provenance. Proposes
    /// nothing and stores nothing; a PDF also yields an editable DOCX
    /// reconstruction in `--out` that needs its own approval.
    Intake {
        /// The reference file (DOCX or readable PDF).
        file: PathBuf,
        /// Existing directory for the reconstruction DOCX (PDF only).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Render a labelled SAMPLE preview of a draft or approved version with
    /// placeholder values, to DOCX and PDF in `--out`. Stores nothing.
    Sample {
        name: String,
        #[arg(long)]
        version: i64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Approve exactly one version (operator connection only). `--sha256`
    /// must be the content digest `template show` printed for it.
    Approve {
        name: String,
        #[arg(long)]
        version: i64,
        #[arg(long)]
        sha256: String,
        /// Who decided and where (e.g. "chris in chat 22:33Z").
        #[arg(long)]
        source: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum DocumentAction {
    /// Create a draft from an approved template version. Validates the data
    /// and computes totals in code. Issues no invoice number and sends
    /// nothing.
    Create {
        /// The approved template name.
        template: String,
        #[arg(long)]
        version: i64,
        /// The data JSON: fields and line items (quantities and prices as text).
        #[arg(long)]
        file: PathBuf,
    },
    /// Show a retained draft with its input and computed context.
    Show { id: String },
    /// Export a retained draft to DOCX and PDF files in `--out`. Creates no
    /// invoice, number or send: the files are a preview of the draft.
    Export {
        id: String,
        /// Existing directory the DOCX and PDF are written into.
        #[arg(long)]
        out: PathBuf,
    },
}

/// The DOCX bytes, base64 for the JSON request.
fn read_docx(path: &PathBuf) -> Result<String> {
    use base64::Engine;
    let bytes = std::fs::read(path)
        .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", path.display())))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn read_json(path: &PathBuf) -> Result<Value> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::rejected(format!("{} is not valid JSON: {e}", path.display())))
}

/// Financial templates keep the historical DOCX field. An authoring pack
/// omits `source`; a supplied DOCX is refused before the RPC.
fn optional_docx(spec: &Value, docx: Option<PathBuf>) -> Result<Option<String>> {
    let authoring = spec.get("kind").and_then(Value::as_str) == Some("authoring");
    match (authoring, docx) {
        (true, Some(_)) => Err(Error::rejected("an authoring pack does not accept --docx")),
        (true, None) => Ok(None),
        (false, None) => Err(Error::rejected(
            "a quotation or invoice template requires --docx",
        )),
        (false, Some(path)) => Ok(Some(read_docx(&path)?)),
    }
}

fn template_params(name: &str, spec: Value, source: Option<String>) -> Value {
    let mut params = json!({"name": name, "spec": spec});
    if let Some(source) = source {
        params["source"] = json!(source);
    }
    params
}

pub(super) fn run_template(state_dir: PathBuf, action: TemplateAction) -> Result<i32> {
    let result = match action {
        TemplateAction::Create { name, docx, file } => {
            let spec = read_json(&file)?;
            let source = optional_docx(&spec, docx)?;
            client::rpc(
                &state_dir,
                "business_template_create",
                template_params(&name, spec, source),
            )?
        }
        TemplateAction::Revise {
            name,
            base,
            docx,
            file,
        } => {
            let spec = read_json(&file)?;
            let source = optional_docx(&spec, docx)?;
            let mut params = template_params(&name, spec, source);
            params["base"] = json!(base);
            client::rpc(&state_dir, "business_template_revise", params)?
        }
        TemplateAction::Show { name, version, out } => {
            if out.is_some() && version.is_none() {
                return Err(Error::rejected("template show --out requires --version"));
            }
            let mut params = json!({"name": name});
            if let Some(version) = version {
                params["version"] = json!(version);
            }
            let result = client::rpc(&state_dir, "business_template_show", params)?;
            match out {
                Some(out) => materialise_authoring_pack(&out, result)?,
                None => result,
            }
        }
        TemplateAction::Intake { file, out } => {
            use base64::Engine;
            let bytes = std::fs::read(&file)
                .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", file.display())))?;
            let name = file
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::rejected("the reference file needs a file name"))?
                .to_string();
            let source = base64::engine::general_purpose::STANDARD.encode(bytes);
            let mut result = client::rpc(
                &state_dir,
                "business_template_intake",
                json!({"file": name, "source": source}),
            )?;
            if let Some(encoded) = result["reconstruction_base64"].as_str().map(str::to_string) {
                let out = out.ok_or_else(|| {
                    Error::rejected("a PDF reconstruction needs --out (an existing directory)")
                })?;
                let docx = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| Error::rejected("intake returned invalid DOCX bytes"))?;
                let stem = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("reference");
                let path = out.join(format!("{stem}.reconstruction.docx"));
                std::fs::write(&path, docx)
                    .map_err(|e| Error::rejected(format!("cannot write reconstruction: {e}")))?;
                result["reconstruction_file"] = json!(path);
            }
            if let Some(object) = result.as_object_mut() {
                object.remove("reconstruction_base64");
            }
            result
        }
        TemplateAction::Sample { name, version, out } => {
            use base64::Engine;
            let result = client::rpc(
                &state_dir,
                "business_template_sample",
                json!({"name": name, "version": version}),
            )?;
            let engine = base64::engine::general_purpose::STANDARD;
            let docx = engine
                .decode(result["docx_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("sample returned invalid DOCX bytes"))?;
            let pdf = engine
                .decode(result["pdf_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("sample returned invalid PDF bytes"))?;
            let docx_path = out.join(format!("{name}-v{version}-sample.docx"));
            let pdf_path = out.join(format!("{name}-v{version}-sample.pdf"));
            std::fs::write(&docx_path, docx)
                .and_then(|_| std::fs::write(&pdf_path, pdf))
                .map_err(|e| Error::rejected(format!("cannot write sample: {e}")))?;
            json!({"sample": true, "docx": docx_path, "pdf": pdf_path, "context": result["context"]})
        }
        TemplateAction::Approve {
            name,
            version,
            sha256,
            source,
        } => client::rpc(
            &state_dir,
            "business_template_approve",
            json!({"name": name, "version": version, "digest": sha256, "source": source}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run_document(state_dir: PathBuf, action: DocumentAction) -> Result<i32> {
    let result = match action {
        DocumentAction::Create {
            template,
            version,
            file,
        } => client::rpc(
            &state_dir,
            "business_document_create",
            json!({"template": template, "version": version, "data": read_json(&file)?}),
        )?,
        DocumentAction::Show { id } => {
            client::rpc(&state_dir, "business_document_show", json!({"id": id}))?
        }
        DocumentAction::Export { id, out } => {
            use base64::Engine;
            let result = client::rpc(&state_dir, "business_document_export", json!({"id": id}))?;
            let engine = base64::engine::general_purpose::STANDARD;
            let docx = engine
                .decode(result["docx_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("export returned invalid DOCX bytes"))?;
            let pdf = engine
                .decode(result["pdf_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("export returned invalid PDF bytes"))?;
            let docx_path = out.join(format!("{id}.docx"));
            let pdf_path = out.join(format!("{id}.pdf"));
            std::fs::write(&docx_path, docx)
                .and_then(|_| std::fs::write(&pdf_path, pdf))
                .map_err(|e| Error::rejected(format!("cannot write export: {e}")))?;
            json!({"id": id, "docx": docx_path, "pdf": pdf_path})
        }
    };
    print_json(&result);
    Ok(0)
}

#[derive(serde::Serialize)]
struct PackManifest<'a> {
    template: &'a Value,
    spec: &'a cadence_agent::business_templates::TemplateDefinition,
}

/// Validate once through the model, then materialise data into a fresh
/// private directory. Reference contents are never interpreted or executed.
fn materialise_authoring_pack(out: &Path, mut show: Value) -> Result<Value> {
    use base64::Engine;
    use std::io::Write;

    let definition = cadence_agent::business_templates::parse_definition(show["spec"].take())?;
    let Some(pack) = definition.authoring() else {
        return Err(Error::rejected(format!(
            "template show --out materialises authoring packs, not '{}'",
            definition.kind()
        )));
    };
    let mut dir = FreshDir::create(out)?;
    let result = (|| -> Result<()> {
        dir.create_file("guidelines.md")?
            .write_all(pack.guidelines.as_bytes())
            .map_err(|e| Error::rejected(format!("cannot write guidelines: {e}")))?;
        for reference in &pack.references {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&reference.content_base64)
                .map_err(|e| Error::rejected(format!("cannot decode reference: {e}")))?;
            dir.create_file(&reference.name)?
                .write_all(&bytes)
                .map_err(|e| Error::rejected(format!("cannot write reference: {e}")))?;
        }
        let manifest = PackManifest {
            template: &show["template"],
            spec: &definition,
        };
        serde_json::to_writer_pretty(dir.create_file("template.json")?, &manifest)
            .map_err(|e| Error::rejected(format!("cannot write template manifest: {e}")))?;
        Ok(())
    })();
    if let Err(error) = result {
        dir.abort();
        return Err(error);
    }
    let references: Vec<_> = pack.references.iter().map(|r| out.join(&r.name)).collect();
    Ok(json!({
        "template": show["template"],
        "out": out,
        "guidelines": out.join("guidelines.md"),
        "references": references,
        "template_json": out.join("template.json"),
    }))
}

fn leaf_cstring(name: &str) -> Result<CString> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.bytes().any(|b| b == 0 || b == b'/' || b == b'\\')
        || Path::new(name).file_name().and_then(|s| s.to_str()) != Some(name)
    {
        return Err(Error::rejected(format!(
            "refusing to write '{name}' outside the new directory"
        )));
    }
    CString::new(name).map_err(|_| Error::rejected("path component contains NUL"))
}

fn io_pack_err(context: &str, err: std::io::Error) -> Error {
    if err.raw_os_error() == Some(libc::ELOOP) {
        Error::rejected(format!("{context}: refusing to follow a symlink"))
    } else {
        Error::rejected(format!("{context}: {err}"))
    }
}

fn open_dir_path(path: &Path) -> Result<OwnedFd> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::rejected("path contains NUL"))?;
    // SAFETY: `c` is a NUL-terminated path; the returned fd is owned.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io_pack_err(
            &format!("cannot open directory {}", path.display()),
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fd` is a freshly opened descriptor not owned elsewhere.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn openat_dir(dir: &OwnedFd, name: &OsStr) -> Result<OwnedFd> {
    let c = CString::new(name.as_bytes())
        .map_err(|_| Error::rejected("path component contains NUL"))?;
    // SAFETY: `dir` is an open directory fd and `c` is one path component.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io_pack_err(
            &format!("cannot open {}", name.to_string_lossy()),
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fd` is a freshly opened descriptor not owned elsewhere.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Walk `path` with `O_NOFOLLOW` on every component, including `.` and `/`.
fn open_nofollow_dir(path: &Path) -> Result<OwnedFd> {
    use std::path::Component;
    let mut components = path.components().peekable();
    if components.peek().is_none() {
        return open_dir_path(Path::new("."));
    }
    let first = components.next().expect("peeked");
    let mut fd = match first {
        Component::RootDir => open_dir_path(Path::new("/"))?,
        Component::CurDir => open_dir_path(Path::new("."))?,
        Component::ParentDir => openat_dir(&open_dir_path(Path::new("."))?, OsStr::new(".."))?,
        Component::Normal(name) => openat_dir(&open_dir_path(Path::new("."))?, name)?,
        Component::Prefix(_) => {
            return Err(Error::rejected("template show --out path is not usable"));
        }
    };
    for component in components {
        fd = match component {
            Component::CurDir => fd,
            Component::Normal(name) => openat_dir(&fd, name)?,
            Component::ParentDir => openat_dir(&fd, OsStr::new(".."))?,
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::rejected("template show --out path is not usable"));
            }
        };
    }
    Ok(fd)
}

/// A directory this process just created. Dropping it leaves the files in
/// place; `abort` removes only the names this process created.
struct FreshDir {
    parent: OwnedFd,
    dir: OwnedFd,
    name: CString,
    files: Vec<CString>,
}

impl FreshDir {
    fn create(out: &Path) -> Result<Self> {
        let leaf = out.file_name().ok_or_else(|| {
            Error::rejected(format!(
                "template show --out needs a new directory name, not {}",
                out.display()
            ))
        })?;
        if leaf == "." || leaf == ".." {
            return Err(Error::rejected(
                "template show --out needs a new directory name",
            ));
        }
        let parent_path = match out.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let parent = open_nofollow_dir(parent_path).map_err(|err| {
            if err.to_string().contains("No such file") {
                Error::rejected(format!(
                    "the parent of {} must already exist",
                    out.display()
                ))
            } else {
                err
            }
        })?;
        let name = CString::new(leaf.as_bytes())
            .map_err(|_| Error::rejected("output directory name contains NUL"))?;
        // SAFETY: `parent` is an open directory and `name` is one component.
        // mkdirat creates exclusively; an existing file or link gives EEXIST.
        // 0700, not 0755: the pack can hold confidential company references.
        let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EEXIST) {
                return Err(Error::rejected(format!(
                    "refusing to overwrite {}; template show --out requires a directory that does not exist",
                    out.display()
                )));
            }
            return Err(io_pack_err(
                &format!("cannot create {}", out.display()),
                err,
            ));
        }
        let dir = openat_dir(&parent, leaf)?;
        // mkdirat masks with the process umask. Force owner-only access.
        // SAFETY: `dir` is the directory this process just created.
        let chmod_rc = unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) };
        if chmod_rc != 0 {
            let err = std::io::Error::last_os_error();
            // SAFETY: removes only the empty directory this process created.
            unsafe {
                libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR);
            }
            return Err(io_pack_err(
                &format!("cannot restrict permissions on {}", out.display()),
                err,
            ));
        }
        Ok(Self {
            parent,
            dir,
            name,
            files: Vec::new(),
        })
    }

    fn create_file(&mut self, name: &str) -> Result<std::fs::File> {
        let c = leaf_cstring(name)?;
        // SAFETY: `O_EXCL|O_NOFOLLOW` creates one regular file and refuses a
        // symlink or an existing name. The fd is owned immediately below.
        let fd = unsafe {
            libc::openat(
                self.dir.as_raw_fd(),
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EEXIST) {
                return Err(Error::rejected(format!(
                    "refusing to overwrite {name} in the output directory"
                )));
            }
            return Err(io_pack_err(&format!("cannot create {name}"), err));
        }
        self.files.push(c);
        // SAFETY: `fd` is a freshly opened descriptor not owned elsewhere.
        Ok(std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn abort(self) {
        for name in &self.files {
            // SAFETY: unlinks a basename this process created in `dir`.
            unsafe {
                libc::unlinkat(self.dir.as_raw_fd(), name.as_ptr(), 0);
            }
        }
        // SAFETY: removes the directory this process created, not a symlink.
        unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            );
        }
    }
}
