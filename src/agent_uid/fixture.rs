//! `FixtureHost` — the test double for [`View`]/[`Host`]. Every path
//! stays under `root` (a tempdir): directories and file contents are
//! real so reads/walks behave; uid/gid/mode and ACLs are recorded in
//! `meta`/`acl` because a test process may not `chown`. The user/group
//! database is in-memory maps — provision's userdb ops land there.
//!
//! Deliberately `pub` under `#[doc(hidden)]`: integration tests drive
//! the whole provision→audit lane against it.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use super::{AclEntry, Group, Host, Meta, NewUser, Principal, User, View, OPERATOR_USER};

pub struct FixtureHost {
    /// The fake filesystem root — a tempdir.
    pub root: PathBuf,
    pub users: BTreeMap<String, User>,
    pub groups: BTreeMap<String, Group>,
    /// Logical path → asserted ownership/mode.
    pub meta: BTreeMap<String, Meta>,
    /// Logical path → ACL entries (access + default).
    pub acl: BTreeMap<String, Vec<AclEntry>>,
    pub env: BTreeMap<String, String>,
    /// The calling process's (real, effective) uid.
    pub uid: u32,
    pub euid: u32,
    /// Uids the write verbs trust — root, the operator, and the real
    /// euid (helper sources live in a runner-owned tempdir). Tests
    /// remove/add entries to forge untrusted chain owners.
    pub trusted_uids: BTreeSet<u32>,
    /// Paths whose write verbs must fail — the apply-error injection
    /// knob, so a test can fail the plan's *last* action on purpose.
    pub fail_ops: BTreeSet<String>,
    next_uid: u32,
    next_gid: u32,
}

impl FixtureHost {
    /// A bare host: `root` (uid 0), the operator account (uid 1000,
    /// group `ubuntu`), nothing provisioned.
    pub fn new(root: &Path) -> Self {
        let mut h = FixtureHost {
            root: root.to_path_buf(),
            users: BTreeMap::new(),
            groups: BTreeMap::new(),
            meta: BTreeMap::new(),
            acl: BTreeMap::new(),
            env: BTreeMap::new(),
            uid: 0,
            euid: 0,
            trusted_uids: {
                let mut t = BTreeSet::from([0, 1000]);
                #[cfg(unix)]
                t.insert(unsafe { libc::geteuid() });
                t
            },
            fail_ops: BTreeSet::new(),
            next_uid: 900,
            next_gid: 900,
        };
        h.users.insert(
            "root".into(),
            User {
                name: "root".into(),
                uid: 0,
                gid: 0,
                home: "/root".into(),
                shell: "/bin/bash".into(),
                locked: Some(true),
            },
        );
        h.groups.insert(
            "root".into(),
            Group {
                name: "root".into(),
                gid: 0,
                members: BTreeSet::new(),
            },
        );
        h.add_user_record(OPERATOR_USER, 1000, 1000, "/home/ubuntu", "/bin/bash");
        h.groups.insert(
            OPERATOR_USER.into(),
            Group {
                name: OPERATOR_USER.into(),
                gid: 1000,
                members: BTreeSet::from([OPERATOR_USER.to_string()]),
            },
        );
        h.seed_dir("/home/ubuntu", 1000, 1000, 0o750);
        h
    }

    /// Insert or overwrite a user record — tests use this to seed
    /// drift (a uid-0 agent, a wrong shell).
    pub fn add_user_record(&mut self, name: &str, uid: u32, gid: u32, home: &str, shell: &str) {
        self.users.insert(
            name.to_string(),
            User {
                name: name.into(),
                uid,
                gid,
                home: home.into(),
                shell: shell.into(),
                locked: Some(true),
            },
        );
    }

    /// Record metadata for a path and create a real directory for it
    /// under the root, so walks and reads behave.
    pub fn seed_dir(&mut self, path: &str, uid: u32, gid: u32, mode: u32) {
        std::fs::create_dir_all(self.phys(path)).unwrap();
        self.meta.insert(
            path.to_string(),
            Meta {
                uid,
                gid,
                mode,
                is_dir: true,
                is_file: false,
                is_symlink: false,
            },
        );
    }

    /// Record metadata for a path and write real bytes under the root.
    pub fn seed_file(&mut self, path: &str, uid: u32, gid: u32, mode: u32, content: &[u8]) {
        let phys = self.phys(path);
        if let Some(p) = phys.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(&phys, content).unwrap();
        self.meta.insert(
            path.to_string(),
            Meta {
                uid,
                gid,
                mode,
                is_dir: false,
                is_file: true,
                is_symlink: false,
            },
        );
    }

    /// Plant a real symlink at `path` under the root — the adversarial
    /// seed: every root write verb must refuse to touch it or anything
    /// beneath it.
    pub fn seed_symlink(&mut self, path: &str, target: &str) -> io::Result<()> {
        let phys = self.phys(path);
        if let Some(p) = phys.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::os::unix::fs::symlink(target, &phys)?;
        self.meta.insert(
            path.to_string(),
            Meta {
                uid: 1000,
                gid: 1000,
                mode: 0o777,
                is_dir: false,
                is_file: false,
                is_symlink: true,
            },
        );
        Ok(())
    }

    /// Record an ACL entry on a path — the negative-assertion tests
    /// plant agent-granting entries here.
    pub fn seed_acl(&mut self, path: &str, entry: AclEntry) {
        self.acl.entry(path.to_string()).or_default().push(entry);
    }

    /// A file under the root carrying real content but no recorded
    /// meta (gitconfig fixtures — the parser reads real bytes).
    pub fn write_file(&mut self, path: &str, content: &str) {
        let phys = self.phys(path);
        if let Some(p) = phys.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(&phys, content).unwrap();
    }

    fn phys(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    fn logical(&self, phys: &Path) -> String {
        match phys.strip_prefix(&self.root) {
            Ok(rel) => format!("/{}", rel.display()),
            Err(_) => phys.display().to_string(),
        }
    }

    /// Mirror of the live chain walk: lstat every component of `phys`;
    /// a symlink anywhere refuses, and every *intermediate*'s recorded
    /// owner must sit inside `trusted_uids` — the last component's
    /// owner is exempt the way the live `O_NOFOLLOW` pin exempts the
    /// pinned target itself (its owner is the spec's, e.g. agent-owned
    /// `repos`, or about to be repaired).
    fn check_chain(&self, phys: &Path) -> io::Result<()> {
        let rel = match phys.strip_prefix(&self.root) {
            Ok(r) => r,
            Err(_) => return Ok(()), // outside the fake root: real fs, no judgement
        };
        let comps: Vec<_> = rel.components().collect();
        let mut cur = self.root.clone();
        for (i, c) in comps.iter().enumerate() {
            cur.push(c);
            let md = match std::fs::symlink_metadata(&cur) {
                Ok(md) => md,
                Err(_) => break,
            };
            if md.file_type().is_symlink() {
                return Err(io::Error::other(format!(
                    "{}: symlink in the path — refusing",
                    self.logical(&cur)
                )));
            }
            let logical = self.logical(&cur);
            if i + 1 < comps.len() {
                if let Some(m) = self.meta.get(&logical) {
                    if !self.trusted_uids.contains(&m.uid) {
                        return Err(io::Error::other(format!(
                            "{logical}: owned by an untrusted uid — refusing to descend"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// The live source gate judged on recorded meta first, real fs
    /// second — a test can forge an agent-owned or group-writable
    /// helper without `chown`.
    fn check_source_meta(&self, src: &Path) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::symlink_metadata(src)?;
        let key = src.display().to_string();
        let (is_file, fmode, uid) = match self.meta.get(&key) {
            Some(m) => (m.is_file, m.mode, m.uid),
            None => (
                md.is_file() && !md.file_type().is_symlink(),
                md.mode() & 0o7777,
                md.uid(),
            ),
        };
        super::check_source(src, is_file, fmode, uid, &self.trusted_uids)
    }

    /// uid/gid a recorded path would report — the fixture's own
    /// authority, never the real fs owner.
    fn alloc_uid(&mut self) -> u32 {
        let u = self.next_uid;
        self.next_uid += 1;
        u
    }
    fn alloc_gid(&mut self) -> u32 {
        let g = self.next_gid;
        self.next_gid += 1;
        g
    }
}

impl View for FixtureHost {
    fn user(&self, name: &str) -> io::Result<Option<User>> {
        Ok(self.users.get(name).cloned())
    }

    fn group(&self, name: &str) -> io::Result<Option<Group>> {
        Ok(self.groups.get(name).cloned())
    }

    fn user_name(&self, uid: u32) -> io::Result<Option<String>> {
        Ok(self
            .users
            .values()
            .find(|u| u.uid == uid)
            .map(|u| u.name.clone()))
    }

    fn group_name(&self, gid: u32) -> io::Result<Option<String>> {
        Ok(self
            .groups
            .values()
            .find(|g| g.gid == gid)
            .map(|g| g.name.clone()))
    }

    fn stat(&self, path: &str) -> io::Result<Option<Meta>> {
        if let Some(m) = self.meta.get(path) {
            return Ok(Some(*m));
        }
        let Ok(md) = std::fs::symlink_metadata(self.phys(path)) else {
            return Ok(None);
        };
        use std::os::unix::fs::MetadataExt;
        Ok(Some(Meta {
            uid: md.uid(),
            gid: md.gid(),
            mode: md.mode() & 0o7777,
            is_dir: md.is_dir(),
            is_file: md.is_file(),
            is_symlink: md.file_type().is_symlink(),
        }))
    }

    fn acls(&self, path: &str) -> io::Result<Vec<AclEntry>> {
        Ok(self.acl.get(path).cloned().unwrap_or_default())
    }

    fn acl_supported(&self) -> bool {
        true
    }

    fn users(&self) -> io::Result<Vec<User>> {
        Ok(self.users.values().cloned().collect())
    }

    fn member_gids(&self, user: &User) -> io::Result<Vec<u32>> {
        let mut gids: Vec<u32> = self
            .groups
            .values()
            .filter(|g| g.members.contains(&user.name))
            .map(|g| g.gid)
            .collect();
        gids.push(user.gid);
        gids.sort_unstable();
        gids.dedup();
        Ok(gids)
    }

    fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
        super::bounded_read(&self.phys(path), path)
    }

    fn read_link(&self, path: &str) -> io::Result<PathBuf> {
        std::fs::read_link(self.phys(path))
    }

    fn file_sha256(&self, path: &str) -> io::Result<[u8; 32]> {
        super::sha256_path(&self.phys(path))
    }

    fn helper_source(&self, src: &Path) -> io::Result<[u8; 32]> {
        self.check_source_meta(src)?;
        let f = std::fs::File::open(src)?;
        super::sha256_file(&f)
    }

    fn walk(&self, dir: &str, budget: usize) -> io::Result<(Vec<String>, bool)> {
        let mut out = Vec::new();
        let mut truncated = false;
        let mut stack = vec![self.phys(dir)];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for ent in entries.flatten() {
                if out.len() >= budget {
                    truncated = true;
                    break;
                }
                let phys = ent.path();
                out.push(self.logical(&phys));
                if ent.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(phys);
                }
            }
            if truncated {
                break;
            }
        }
        Ok((out, truncated))
    }

    fn env(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned()
    }
}

impl Host for FixtureHost {
    fn as_view(&self) -> &dyn View {
        self
    }

    fn create_group(&mut self, name: &str, system: bool) -> io::Result<()> {
        let _ = system;
        if self.groups.contains_key(name) {
            return Ok(());
        }
        let gid = self.alloc_gid();
        self.groups.insert(
            name.to_string(),
            Group {
                name: name.into(),
                gid,
                members: BTreeSet::new(),
            },
        );
        Ok(())
    }

    fn create_user(&mut self, spec: &NewUser) -> io::Result<()> {
        if self.users.contains_key(&spec.name) {
            return Ok(());
        }
        let uid = self.alloc_uid();
        let gid = self
            .groups
            .get(&spec.primary_group)
            .map(|g| g.gid)
            .ok_or_else(|| {
                io::Error::other(format!("primary group {} absent", spec.primary_group))
            })?;
        self.users.insert(
            spec.name.clone(),
            User {
                name: spec.name.clone(),
                uid,
                gid,
                home: spec.home.clone(),
                shell: spec.shell.clone(),
                locked: Some(true),
            },
        );
        // `useradd -m` lands the home before our own `install -d`
        // asserts its final mode. Never follow a planted link: if the
        // home is already a symlink, `useradd` still succeeds — the
        // recorded meta must keep `is_symlink` so the later Dir
        // action assesses it as the refusal it is.
        let phys = self.phys(&spec.home);
        let planted_link = std::fs::symlink_metadata(&phys)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if !planted_link {
            std::fs::create_dir_all(&phys)?;
        }
        self.meta.insert(
            spec.home.clone(),
            Meta {
                uid,
                gid,
                mode: 0o755,
                is_dir: phys.is_dir(),
                is_file: false,
                is_symlink: planted_link,
            },
        );
        Ok(())
    }

    fn repair_user(&mut self, spec: &NewUser) -> io::Result<()> {
        let gid = self
            .groups
            .get(&spec.primary_group)
            .map(|g| g.gid)
            .ok_or_else(|| {
                io::Error::other(format!("primary group {} absent", spec.primary_group))
            })?;
        let u = self
            .users
            .get_mut(&spec.name)
            .ok_or_else(|| io::Error::other(format!("no such user: {}", spec.name)))?;
        u.home = spec.home.clone();
        u.shell = spec.shell.clone();
        u.gid = gid;
        Ok(())
    }

    fn add_member(&mut self, group: &str, user: &str) -> io::Result<()> {
        // `usermod` fails on an absent user — so does the fixture.
        if !self.users.contains_key(user) {
            return Err(io::Error::other(format!("no such user: {user}")));
        }
        let g = self
            .groups
            .get_mut(group)
            .ok_or_else(|| io::Error::other(format!("no such group: {group}")))?;
        g.members.insert(user.to_string());
        Ok(())
    }

    fn mkdir(&mut self, path: &str) -> io::Result<()> {
        if self.fail_ops.contains(path) {
            return Err(io::Error::other(format!("{path}: injected failure")));
        }
        let phys = self.phys(path);
        // The parent chain gets the same refusal the live walk gives:
        // a planted symlink or an untrusted owner blocks the create.
        self.check_chain(&phys)?;
        std::fs::create_dir_all(&phys)?;
        let md = std::fs::symlink_metadata(&phys)?;
        if md.file_type().is_symlink() {
            return Err(io::Error::other(format!("{path}: symlink — refusing")));
        }
        Ok(())
    }

    fn set_meta(&mut self, path: &str, owner: &str, group: &str, mode: u32) -> io::Result<()> {
        let uid = self
            .users
            .get(owner)
            .map(|u| u.uid)
            .ok_or_else(|| io::Error::other(format!("no such user: {owner}")))?;
        let gid = self
            .groups
            .get(group)
            .map(|g| g.gid)
            .ok_or_else(|| io::Error::other(format!("no such group: {group}")))?;
        if self.fail_ops.contains(path) {
            return Err(io::Error::other(format!("{path}: injected failure")));
        }
        let phys = self.phys(path);
        self.check_chain(&phys)?;
        let entry = self.meta.entry(path.to_string()).or_insert(Meta {
            uid,
            gid,
            mode: 0,
            is_dir: phys.is_dir(),
            is_file: phys.is_file(),
            is_symlink: false,
        });
        entry.uid = uid;
        entry.gid = gid;
        entry.mode = mode;
        entry.is_dir = phys.is_dir();
        entry.is_file = phys.is_file();
        Ok(())
    }

    fn install(
        &mut self,
        src: &Path,
        dest: &str,
        owner: &str,
        group: &str,
        mode: u32,
    ) -> io::Result<()> {
        if !src.is_absolute() {
            return Err(io::Error::other(format!(
                "{}: the helper source must be an absolute path — under sudo the cwd is untrusted",
                src.display()
            )));
        }
        self.check_source_meta(src)?;
        if self.fail_ops.contains(dest) {
            return Err(io::Error::other(format!("{dest}: injected failure")));
        }
        let phys = self.phys(dest);
        // Judge the whole chain *before* creating under it — a
        // planted link or an untrusted intermediate refuses rather
        // than being followed by `create_dir_all`.
        self.check_chain(&phys)?;
        if let Some(p) = phys.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut rf = std::fs::File::open(src)?;
        // Read-write: `copy_verified` re-reads the destination fd to
        // prove what landed matches what left the source.
        let wf = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&phys)?;
        super::copy_verified(&mut rf, &wf)?;
        self.set_meta(dest, owner, group, mode)
    }

    fn set_default_group_acl(&mut self, path: &str, group: &str, perms: u8) -> io::Result<()> {
        let gid = self
            .groups
            .get(group)
            .map(|g| g.gid)
            .ok_or_else(|| io::Error::other(format!("no such group: {group}")))?;
        let entries = self.acl.entry(path.to_string()).or_default();
        entries.retain(|e| !(e.default && e.tag == Principal::Group(gid)));
        entries.push(AclEntry {
            default: true,
            tag: Principal::Group(gid),
            perms,
        });
        Ok(())
    }
}
