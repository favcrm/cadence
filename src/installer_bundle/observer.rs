//! Host-only bounded procfs diagnostics. No fact/nonce elects authority.
//! Remote securebits/keepcaps are NOT exposed by procfs: never invent them.
use super::{carrier, files, fixed_arguments, production_image, refused, read_host_frame, FrameKind, Deadline, QualifiedImage, Result, IDS};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Namespaces { user:(u64,u64), pid:(u64,u64), mount:(u64,u64) }
#[derive(Clone, Debug, PartialEq, Eq)]
struct Status { uids:[u32;4], gids:[u32;4], groups:Vec<u32>, caps:[u64;5], nnp:u32, pid:u32, tgid:u32, threads:u32 }
fn numbers(value:&str)->Result<Vec<u32>> {
    value.split_ascii_whitespace().map(|s| {
        if s.is_empty() || !s.bytes().all(|b|b.is_ascii_digit()) {return Err(refused());}
        s.parse().map_err(|_|refused())
    }).collect()
}
fn field<'a>(text:&'a str,key:&str)->Result<&'a str> {
    let mut values=text.lines().filter_map(|line|line.strip_prefix(key));
    let out=values.next().ok_or_else(refused)?;
    if values.next().is_some() {return Err(refused());} Ok(out.trim())
}
fn status(text:&str)->Result<Status> {
    if text.len()>65536 {return Err(refused());}
    let uids=numbers(field(text,"Uid:")?)?.try_into().map_err(|_|refused())?;
    let gids=numbers(field(text,"Gid:")?)?.try_into().map_err(|_|refused())?;
    let groups=numbers(field(text,"Groups:")?)?;
    let mut caps=[0;5];
    for (i,key) in ["CapInh:","CapPrm:","CapEff:","CapBnd:","CapAmb:"].iter().enumerate() {
        let s=field(text,key)?;
        if s.len()!=16 || !s.bytes().all(|b|b.is_ascii_hexdigit()) {return Err(refused());}
        caps[i]=u64::from_str_radix(s,16).map_err(|_|refused())?;
    }
    let single=|key|->Result<u32> {let v=numbers(field(text,key)?)?;if v.len()!=1 {return Err(refused());}Ok(v[0])};
    Ok(Status {uids,gids,groups,caps,nnp:single("NoNewPrivs:")?,pid:single("Pid:")?,tgid:single("Tgid:")?,threads:single("Threads:")?})
}
fn sealed(status:&Status,pid:u32)->Result<()> {
    if status.uids!=[IDS;4] || status.gids!=[IDS;4] || !status.groups.is_empty() || status.caps!=[0;5]
        || status.nnp!=1 || status.pid!=pid || status.tgid!=pid || status.threads!=1 {return Err(refused());} Ok(())
}
pub(super) fn sealed_self()->Result<()> {
    let mut text=String::new();
    File::open("/proc/self/status").map_err(|_|refused())?.take(65537)
        .read_to_string(&mut text).map_err(|_|refused())?;
    sealed(&status(&text)?,std::process::id())
}
fn open(dir:&File,name:&str,flags:i32)->Result<File> {
    let name=CString::new(name).map_err(|_|refused())?;
    let fd=unsafe {libc::openat(dir.as_raw_fd(),name.as_ptr(),flags|libc::O_CLOEXEC)};
    if fd<0 {return Err(refused());} Ok(unsafe {File::from_raw_fd(fd)})
}
fn bounded(dir:&File,name:&str,limit:usize,deadline:Deadline)->Result<String> {
    deadline.check()?;
    let mut out=Vec::new();
    open(dir,name,libc::O_RDONLY|libc::O_NOFOLLOW|libc::O_NONBLOCK)?.take((limit+1) as u64)
        .read_to_end(&mut out).map_err(|_|refused())?;
    deadline.check()?;
    if out.len()>limit {return Err(refused());} String::from_utf8(out).map_err(|_|refused())
}
fn namespace(dir:&File,name:&str)->Result<(u64,u64)> {
    // Only fixed kernel namespace links beneath the held PID directory.
    let f=open(dir,name,libc::O_RDONLY)?;
    let m=f.metadata().map_err(|_|refused())?;Ok((m.dev(),m.ino()))
}
fn namespaces(dir:&File)->Result<Namespaces> {
    Ok(Namespaces {user:namespace(dir,"ns/user")?,pid:namespace(dir,"ns/pid")?,mount:namespace(dir,"ns/mnt")?})
}
pub(super) struct Diagnostic {
    pid:u32, starttime:u64, state:Status, namespaces:Namespaces, executable:files::Stamp, client_digest:[u8;32],
    // No process-local PR_GET_SECUREBITS can observe another process.
    securebits:Option<u32>, keepcaps:Option<u32>,
    // Keep actual inode/exec handles THROUGH peer/owner/consume rechecks.
    held_proc:File, held_pid:File, held_exe:File, protected:files::HeldArtifact,
    pid_stamp:files::Stamp,
}
impl Diagnostic {
    pub(super) fn starttime(&self)->u64 {self.starttime}
    pub(super) fn recheck(&self,image:&QualifiedImage,deadline:Deadline)->Result<()> {
        if files::stamp(&self.held_pid)?!=self.pid_stamp || files::stamp(&self.held_exe)?!=self.executable
            || start(&self.held_pid,self.pid,deadline)?!=self.starttime {return Err(refused());}
        let dir=open(&self.held_proc,&self.pid.to_string(),libc::O_RDONLY|libc::O_DIRECTORY|libc::O_NOFOLLOW)?;
        let exe=open(&dir,"exe",libc::O_RDONLY|libc::O_NONBLOCK)?;
        if files::stamp(&dir)?!=self.pid_stamp || files::stamp(&exe)?!=self.executable
            || files::measure(&exe,image.client,0,deadline)?!=self.executable
            || start(&dir,self.pid,deadline)?!=self.starttime
            || status(&bounded(&dir,"status",65536,deadline)?)?!=self.state
            || namespaces(&dir)?!=self.namespaces {return Err(refused());}
        self.protected.recheck(deadline)?;deadline.check()
    }
    pub(super) fn require_release_measurement(&self)->Result<()> {
        if self.securebits!=Some(super::seal::SECUREBITS) || self.keepcaps!=Some(0) {return Err(refused());}
        Ok(())
    }
    pub(super) fn require_construction_measurement(&self)->Result<()> {
        // Self construction may read its own prctl state. This is NOT the
        // external enrollment observer and cannot prove earlier root origin.
        if self.pid!=std::process::id() || unsafe {libc::prctl(libc::PR_GET_SECUREBITS,0,0,0,0)} != super::seal::SECUREBITS as i32
            || unsafe {libc::prctl(libc::PR_GET_KEEPCAPS,0,0,0,0)} != 0
            || unsafe {libc::prctl(libc::PR_GET_NO_NEW_PRIVS,0,0,0,0)} != 1 {return Err(refused());}
        Ok(())
    }
    fn output(&self, operation:&str,barrier:&str)->Result<Vec<u8>> {
        // Diagnostic only. Missing securebits remains null, NEVER qualified.
        let mut out=serde_json::to_vec(&serde_json::json!({
            "version":1,"operation":operation,"barrierNonce":barrier,
            "pid":self.pid,"starttime":self.starttime.to_string(),
            "clientDigest":self.client_digest.iter().map(|b|format!("{b:02x}")).collect::<String>(),
            "uid":self.state.uids,"gid":self.state.gids,"groups":self.state.groups,
            "caps":self.state.caps,"threads":self.state.threads,
            "noNewPrivs":self.state.nnp,"securebits":self.securebits,"keepcaps":self.keepcaps,
            "namespaces":{"user":[self.namespaces.user.0.to_string(),self.namespaces.user.1.to_string()],
                "pid":[self.namespaces.pid.0.to_string(),self.namespaces.pid.1.to_string()],
                "mount":[self.namespaces.mount.0.to_string(),self.namespaces.mount.1.to_string()]}
        })).map_err(|_|refused())?;
        out.push(b'\n');if out.len()>4096 {return Err(refused());} Ok(out)
    }
}
fn start(dir:&File,pid:u32,deadline:Deadline)->Result<u64> {
    let s=bounded(dir,"stat",4096,deadline)?;
    if s.split_once(' ').and_then(|(v,_)|v.parse::<u32>().ok())!=Some(pid) {return Err(refused());}
    crate::peer::parse_proc_starttime(&s).ok_or_else(refused)
}
pub(super) fn observe(pid:u32,image:&QualifiedImage,deadline:Deadline)->Result<Diagnostic> {
    if pid==0 || pid>4194304 {return Err(refused());}
    let root=File::open("/proc").map_err(|_|refused())?;
    let mut fs:libc::statfs=unsafe {std::mem::zeroed()};
    if unsafe {libc::fstatfs(root.as_raw_fd(),&mut fs)}!=0 || fs.f_type!=libc::PROC_SUPER_MAGIC {return Err(refused());}
    let dir=open(&root,&pid.to_string(),libc::O_RDONLY|libc::O_DIRECTORY|libc::O_NOFOLLOW)?;
    let initial_dir=files::stamp(&dir)?;
    let a=start(&dir,pid,deadline)?;
    let status_a=status(&bounded(&dir,"status",65536,deadline)?)?;sealed(&status_a,pid)?;
    let ns_a=namespaces(&dir)?;
    if ns_a!=image.namespaces {return Err(refused());}
    let exe=open(&dir,"exe",libc::O_RDONLY|libc::O_NONBLOCK)?;
    let selected=files::HeldArtifact::open(files::Artifact::Client,image.client,deadline)?;
    let measured=files::measure(&exe,image.client,0,deadline)?;
    files::correspond(&exe,selected.selected_stamp())?;
    let exe_again=open(&dir,"exe",libc::O_RDONLY|libc::O_NONBLOCK)?;
    let dir_again=open(&root,&pid.to_string(),libc::O_RDONLY|libc::O_DIRECTORY|libc::O_NOFOLLOW)?;
    if files::stamp(&exe_again)?!=measured || files::stamp(&exe)?!=measured
        || files::stamp(&dir_again)?!=initial_dir || start(&dir_again,pid,deadline)?!=a
        || status(&bounded(&dir_again,"status",65536,deadline)?)?!=status_a || namespaces(&dir_again)?!=ns_a {return Err(refused());}
    selected.recheck(deadline)?;
    deadline.check()?;
    Ok(Diagnostic {pid,starttime:a,state:status_a,namespaces:ns_a,executable:measured,client_digest:image.client,securebits:None,keepcaps:None,
        held_proc:root,held_pid:dir,held_exe:exe,protected:selected,pid_stamp:initial_dir})
}
fn uuid(s:&str)->bool {
    s.len()==36 && s.bytes().enumerate().all(|(i,b)| if [8,13,18,23].contains(&i) {b==b'-'}else{b.is_ascii_digit()||(b'a'..=b'f').contains(&b)})
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {version:u8,pid:u32,operation:String,#[serde(rename="barrierNonce")]barrier:String}
fn input(bytes:&[u8])->Result<Input> {
    if bytes.len()>256 {return Err(refused());}
    let value:Input=serde_json::from_slice(bytes).map_err(|_|refused())?;
    if value.version!=1 || value.pid==0 || value.pid>4194304 || !uuid(&value.operation) || !uuid(&value.barrier) {return Err(refused());} Ok(value)
}
pub(super) fn entry()->Result<()> {
    fixed_arguments()?;
    if carrier::ids()?!=[0;6] {return Err(refused());}
    let image=production_image()?; // before any input/procfs target selection
    let deadline=Deadline::new();
    let selected=files::HeldArtifact::open(files::Artifact::Observer,image.observer,deadline)?;
    selected.self_correspondence(deadline)?;
    let stdin=std::io::stdin();let request=read_host_frame(stdin.as_raw_fd(),deadline,FrameKind::Observation)?;
    let input=input(&request.0)?;
    let facts=observe(input.pid,&image,deadline)?;
    // Output is correlated procfs DIAGNOSTIC, not an enrollment or release.
    let out=facts.output(&input.operation,&input.barrier)?;
    // Finite nonblocking output; never let an unowned slow pipe hold observer.
    let fd=std::io::stdout().as_raw_fd();
    let old=unsafe {libc::fcntl(fd,libc::F_GETFL)};
    if old<0 || unsafe {libc::fcntl(fd,libc::F_SETFL,old|libc::O_NONBLOCK)}<0 {return Err(refused());}
    deadline.check()?;
    let mut stdout=std::io::stdout();
    let written=stdout.write_all(&out).map_err(|_|refused());
    let restored=unsafe {libc::fcntl(fd,libc::F_SETFL,old)};
    deadline.check()?;
    if restored<0 {return Err(refused());}
    written?;
    // Diagnostic output is explicitly incomplete; unknown target securebits
    // cannot be a successful enrollment/qualified observation.
    facts.require_release_measurement()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn good()->String { format!("Uid:\t21000 21000 21000 21000\nGid:\t21000 21000 21000 21000\nGroups:\nCapInh:\t{:016x}\nCapPrm:\t{:016x}\nCapEff:\t{:016x}\nCapBnd:\t{:016x}\nCapAmb:\t{:016x}\nNoNewPrivs:\t1\nPid:\t123\nTgid:\t123\nThreads:\t1\n",0,0,0,0,0) }
    #[test]
    fn proc_status_duplicate_bounds_and_each_actual_seal_field_refuse() {
        let s=good();sealed(&status(&s).unwrap(),123).unwrap();
        for bad in [s.clone()+"Uid: 21000 21000 21000 21000\n", "x".repeat(65537),s.replace("21000 21000 21000 21000","21000 21000 0 21000"),s.replace("Groups:\n","Groups: 21000\n"),s.replace("NoNewPrivs:\t1","NoNewPrivs:\t0"),s.replace("Threads:\t1","Threads:\t2"),s.replace("CapAmb:\t0000000000000000","CapAmb:\t0000000000000001")] {
            assert!(status(&bad).and_then(|s|sealed(&s,123)).is_err());
        }
        assert!(sealed(&status(&s).unwrap(),124).is_err());
    }
    #[test]
    fn observer_closed_input_never_accepts_caller_measurement_or_path() {
        let good=br#"{"version":1,"pid":123,"operation":"11111111-1111-4111-8111-111111111111","barrierNonce":"22222222-2222-4222-8222-222222222222"}"#;
        assert!(input(good).is_ok());
        assert!(input(&[b'x';257]).is_err());
        let with_path=String::from_utf8(good.to_vec()).unwrap().replace("\"version\":1","\"version\":1,\"procfs\":\"/tmp\"");
        assert!(input(with_path.as_bytes()).is_err());
        assert!(input(br#"{"version":1,"version":1,"pid":123}"#).is_err());
    }
}
