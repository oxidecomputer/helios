/*
 * Copyright 2020 Oxide Computer Company
 */

use std::os::raw::{c_char, c_int};
use std::process::{exit, Command};
use std::ffi::{CString, CStr};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::io::Write;
use anyhow::{Result, bail};
use slog::Logger;
use super::common::{OutputExt, sleep};

const PFEXEC: &str = "/bin/pfexec";
const ZONEADM: &str = "/usr/sbin/zoneadm";
const ZONECFG: &str = "/usr/sbin/zonecfg";
const ZLOGIN: &str = "/usr/sbin/zlogin";
const SVCS: &str = "/bin/svcs";

#[derive(Debug, PartialEq)]
pub struct UserAttr {
    pub name: String,
    pub attr: HashMap<String, String>,
}

impl UserAttr {
    pub fn profiles(&self) -> Vec<String> {
        if let Some(p) = self.attr.get("profiles") {
            p.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    }
}

#[repr(C)]
struct Kv {
    key: *const c_char,
    value: *const c_char,
}

impl Kv {
    fn name(&self) -> &CStr {
        unsafe { CStr::from_ptr(self.key) }
    }

    fn value(&self) -> &CStr {
        unsafe { CStr::from_ptr(self.value) }
    }
}

#[repr(C)]
struct Kva {
    length: c_int,
    data: *const Kv,
}

impl Kva {
    fn values(&self) -> &[Kv] {
        unsafe { std::slice::from_raw_parts(self.data, self.length as usize) }
    }
}

#[repr(C)]
struct UserAttrRaw {
    name: *mut c_char,
    qualifier: *mut c_char,
    res1: *mut c_char,
    res2: *mut c_char,
    attr: *mut Kva,
}

#[link(name = "secdb")]
extern {
    fn getusernam(buf: *const c_char) -> *mut UserAttrRaw;
    fn free_userattr(userattr: *mut UserAttrRaw);
}

pub fn get_user_attr_by_name(name: &str) -> Result<Option<UserAttr>> {
    let mut out = UserAttr {
        name: name.to_string(),
        attr: HashMap::new(),
    };

    let name = CString::new(name.to_owned())?;
    let ua = unsafe { getusernam(name.as_ptr()) };
    if ua.is_null() {
        return Ok(None);
    }

    for kv in unsafe { (*(*ua).attr).values() } {
        if let (Ok(k), Ok(v)) = (kv.name().to_str(), kv.value().to_str()) {
            out.attr.insert(k.to_string(), v.to_string());
        } else {
            continue;
        }
    }

    unsafe { free_userattr(ua) };

    Ok(Some(out))
}

pub fn nodename() -> String {
    unsafe {
        let mut un: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut un) < 0 {
            eprintln!("uname failure");
            exit(100);
        }
        std::ffi::CStr::from_ptr(un.nodename.as_mut_ptr())
    }.to_str().unwrap().to_string()
}

#[link(name = "c")]
extern {
    fn getzoneid() -> i32;
    fn getzonenamebyid(id: i32, buf: *mut u8, buflen: usize) -> isize;
}

pub fn zoneid() -> i32 {
    unsafe { getzoneid() }
}

pub fn zonename() -> String {
    let buf = unsafe {
        let mut buf: [u8; 64] = std::mem::zeroed(); /* ZONENAME_MAX */

        let sz = getzonenamebyid(getzoneid(), buf.as_mut_ptr(), 64);
        if sz > 64 || sz < 0 {
            eprintln!("getzonenamebyid failure");
            exit(100);
        }

        Vec::from(&buf[0..sz as usize])
    };
    std::ffi::CStr::from_bytes_with_nul(&buf)
        .unwrap().to_str().unwrap().to_string()
}

fn errno() -> i32 {
    unsafe {
        let enp = libc::___errno();
        *enp
    }
}

fn clear_errno() {
    unsafe {
        let enp = libc::___errno();
        *enp = 0;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Passwd {
    pub name: Option<String>,
    pub passwd: Option<String>,
    pub uid: u32,
    pub gid: u32,
    pub age: Option<String>,
    pub comment: Option<String>,
    pub gecos: Option<String>,
    pub dir: Option<String>,
    pub shell: Option<String>,
}

impl Passwd {
    fn from(p: *const libc::passwd) -> Result<Passwd> {
        fn cs(lpsz: *const c_char) -> Result<Option<String>> {
            if lpsz.is_null() {
                Ok(None)
            } else {
                let cstr = unsafe { CStr::from_ptr(lpsz) };
                Ok(Some(cstr.to_str()?.to_string()))
            }
        }

        Ok(Passwd {
            name: cs(unsafe { (*p).pw_name })?,
            passwd: cs(unsafe { (*p).pw_passwd })?,
            uid: unsafe { (*p).pw_uid },
            gid: unsafe { (*p).pw_gid },
            age: cs(unsafe { (*p).pw_age })?,
            comment: cs(unsafe { (*p).pw_comment })?,
            gecos: cs(unsafe { (*p).pw_gecos })?,
            dir: cs(unsafe { (*p).pw_dir })?,
            shell: cs(unsafe { (*p).pw_shell })?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: Option<String>,
    pub passwd: Option<String>,
    pub gid: u32,
    pub members: Option<Vec<String>>,
}

impl Group {
    fn from(g: *mut libc::group) -> Result<Group> {
        fn cs(lpsz: *const c_char) -> Result<Option<String>> {
            if lpsz.is_null() {
                Ok(None)
            } else {
                let cstr = unsafe { CStr::from_ptr(lpsz) };
                Ok(Some(cstr.to_str()?.to_string()))
            }
        }

        let mut mems = unsafe { (*g).gr_mem };
        let members: Option<Vec<String>> = if !mems.is_null() {
            let mut members = Vec::new();
            loop {
                if unsafe { *mems }.is_null() {
                    break;
                }

                members.push(cs(unsafe { *mems })?.unwrap());

                mems = unsafe { mems.offset(1) };
             }
            Some(members)
        } else {
            None
        };

        Ok(Group {
            name: cs(unsafe { (*g).gr_name })?,
            passwd: cs(unsafe { (*g).gr_passwd })?,
            gid: unsafe { (*g).gr_gid },
            members,
        })
    }
}

pub fn get_passwd_by_id(uid: u32) -> Result<Option<Passwd>> {
    clear_errno();
    let p = unsafe { libc::getpwuid(uid) };
    let e = errno();
    if p.is_null() {
        if e == 0 {
            Ok(None)
        } else {
            bail!("getpwuid: errno {}", e);
        }
    } else {
        Ok(Some(Passwd::from(p)?))
    }
}

pub fn get_passwd_by_name(name: &str) -> Result<Option<Passwd>> {
    clear_errno();
    let name = CString::new(name.to_owned())?;
    let p = unsafe { libc::getpwnam(name.as_ptr()) };
    let e = errno();
    if p.is_null() {
        if e == 0 {
            Ok(None)
        } else {
            bail!("getpwnam: errno {}", e);
        }
    } else {
        Ok(Some(Passwd::from(p)?))
    }
}

pub fn get_group_by_name(name: &str) -> Result<Option<Group>> {
    clear_errno();
    let name = CString::new(name.to_owned())?;
    let g = unsafe { libc::getgrnam(name.as_ptr()) };
    let e = errno();
    if g.is_null() {
        if e == 0 {
            Ok(None)
        } else {
            bail!("getgrnam: errno {}", e);
        }
    } else {
        Ok(Some(Group::from(g)?))
    }
}

pub fn get_group_by_id(gid: u32) -> Result<Option<Group>> {
    clear_errno();
    let g = unsafe { libc::getgrgid(gid) };
    let e = errno();
    if g.is_null() {
        if e == 0 {
            Ok(None)
        } else {
            bail!("getgrgid: errno {}", e);
        }
    } else {
        Ok(Some(Group::from(g)?))
    }
}

pub struct Terms {
    terms: Vec<String>,
    buf: Option<String>,
}

impl Terms {
    fn append(&mut self, c: char) {
        if self.buf.is_none() {
            self.buf = Some(String::new());
        }
        self.buf.as_mut().unwrap().push(c);
    }

    fn commit(&mut self) {
        if let Some(val) = &self.buf {
            self.terms.push(val.to_string());
        }
        self.buf = Some(String::new());
    }

    fn result(&self) -> Vec<String> {
        self.terms.to_owned()
    }

    fn new() -> Terms {
        Terms {
            terms: Vec::new(),
            buf: Some(String::new()),
        }
    }
}

pub fn parse_net_adm(stdout: Vec<u8>) -> Result<Vec<Vec<String>>> {
    let stdout = String::from_utf8(stdout)?;
    let mut out = Vec::new();

    for l in stdout.lines() {
        let mut terms = Terms::new();
        let mut escape = false;

        for c in l.chars() {
            if escape {
                terms.append(c);
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == ':' {
                terms.commit();
            } else {
                terms.append(c);
            }
        }
        terms.commit();

        out.push(terms.result());
    }

    Ok(out)
}

#[derive(Debug, Clone)]
pub struct Zone {
    pub id: Option<u64>,
    pub name: String,
    pub state: String,
    pub path: PathBuf,
    pub uuid: Option<String>,
    pub brand: String,
    pub ip_type: String,
}

pub trait ZonesExt {
    fn by_name(&self, n: &str) -> Result<Zone>;
    fn exists(&self, n: &str) -> bool;
}

impl ZonesExt for Vec<Zone> {
    fn exists(&self, n: &str) -> bool {
        for z in self {
            if n == z.name {
                return true;
            }
        }

        false
    }

    fn by_name(&self, n: &str) -> Result<Zone> {
        for z in self {
            if n == z.name {
                return Ok(z.clone());
            }
        }

        bail!("could not find zone by name \"{}\"", n);
    }
}

pub fn zone_list() -> Result<Vec<Zone>> {
    let out = Command::new("/usr/sbin/zoneadm")
        .env_clear()
        .arg("list")
        .arg("-cip")
        .output()?;

    if !out.status.success() {
        bail!("zoneadm list failure: {}", out.info());
    }

    let mut zones = Vec::new();

    for line in parse_net_adm(out.stdout)?.iter() {
        if line.len() < 7 {
            bail!("invalid zoneadm list line: {:?}", line);
        }

        let id = if line[0] == "-" {
            None
        } else {
            Some(line[0].parse()?)
        };

        let uuid = if line[4] == "" {
            None
        } else {
            Some(line[4].to_string())
        };

        zones.push(Zone {
            id,
            name: line[1].to_string(),
            state: line[2].to_string(),
            path: PathBuf::from(&line[3]),
            uuid,
            brand: line[5].to_string(),
            ip_type: line[6].to_string(),
        });
    }

    Ok(zones)
}

pub fn zone_create<P, S1, S2>(name: S1, path: P, brand: S2)
    -> Result<()>
    where
        P: AsRef<Path>,
        S1: AsRef<str>,
        S2: AsRef<str>,
{
    let n = name.as_ref();
    let p = path.as_ref();
    let b = brand.as_ref();

    let mut script = String::new();
    script += "create -b; ";
    script += &format!("set zonepath={}; ", p.to_str().unwrap());
    script += &format!("set zonename={}; ", n);
    script += &format!("set brand={}; ", b);
    script += "commit; ";

    println!("args: {}", script);

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONECFG)
        .arg("-z").arg(n)
        .arg(script)
        .output()?;

    if !out.status.success() {
        bail!("zonecfg create failure: {}", out.info());
    }

    Ok(())
}

pub fn zone_add_lofs<P1, P2, S1>(name: S1, gz: P1, ngz: P2)
    -> Result<()>
    where
        P1: AsRef<Path>,
        P2: AsRef<Path>,
        S1: AsRef<str>,
{
    let n = name.as_ref();
    let gz = gz.as_ref();
    let ngz = ngz.as_ref();

    let mut script = String::new();
    script += "add fs; ";
    script += &format!("set dir = {}; ", ngz.to_str().unwrap());
    script += &format!("set special = {}; ", gz.to_str().unwrap());
    script += &format!("set type = lofs; ");
    script += &format!("set options = [rw,nodevices]; ");
    script += "end; ";
    script += "commit; ";

    println!("args: {}", script);

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONECFG)
        .arg("-z").arg(n)
        .arg(script)
        .output()?;

    if !out.status.success() {
        bail!("zonecfg failure: {}", out.info());
    }

    Ok(())
}

pub fn zone_install<S1>(name: S1, packages: &[&str])
    -> Result<()>
    where
        S1: AsRef<str>,
{
    let n = name.as_ref();

    let mut cmd = Command::new(PFEXEC);
    cmd.env_clear();
    cmd.arg(ZONEADM);
    cmd.arg("-z");
    cmd.arg(n);
    cmd.arg("install");

    if !packages.is_empty() {
        cmd.arg("-e");
        for p in packages.iter() {
            cmd.arg(p);
        }
    }

    let mut child = cmd.spawn()?;

    let status = child.wait()?;

    if !status.success() {
        bail!("zoneadm install failure");
    }

    Ok(())
}

pub fn zone_clone<S1, S2>(name: S1, src: S2)
    -> Result<()>
    where
        S1: AsRef<str>,
        S2: AsRef<str>,
{
    let n = name.as_ref();
    let src = src.as_ref();

    let mut cmd = Command::new(PFEXEC);
    cmd.env_clear();
    cmd.arg(ZONEADM);
    cmd.arg("-z");
    cmd.arg(n);
    cmd.arg("clone");
    cmd.arg(src);

    let mut child = cmd.spawn()?;

    let status = child.wait()?;

    if !status.success() {
        bail!("zoneadm clone failure");
    }

    Ok(())
}

pub fn zone_halt<S1>(name: S1)
    -> Result<()>
    where
        S1: AsRef<str>,
{
    let n = name.as_ref();

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONEADM)
        .arg("-z")
        .arg(n)
        .arg("halt")
        .output()?;

    if !out.status.success() {
        bail!("zoneadm halt {} failure: {}", n, out.info());
    }

    Ok(())
}

pub fn zone_boot<S1>(name: S1)
    -> Result<()>
    where
        S1: AsRef<str>,
{
    let n = name.as_ref();

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONEADM)
        .arg("-z")
        .arg(n)
        .arg("boot")
        .output()?;

    if !out.status.success() {
        bail!("zoneadm boot {} failure: {}", n, out.info());
    }

    Ok(())
}

pub fn zone_milestone_wait<S1, S2>(_log: &Logger, name: S1, fmri: S2)
    -> Result<()>
    where
        S1: AsRef<str>,
        S2: AsRef<str>,
{
    let name = name.as_ref();
    let fmri = fmri.as_ref();

    loop {
        let out = Command::new(PFEXEC)
            .env_clear()
            .arg(SVCS)
            .arg("-z")
            .arg(name)
            .arg("-Ho")
            .arg("sta,nsta")
            .arg(fmri)
            .output();

        if let Ok(out) = out {
            let stdout = String::from_utf8(out.stdout)?;
            let lines: Vec<_> = stdout.lines().collect();
            if lines.len() == 1 {
                let t: Vec<&str> = lines[0].split_whitespace().collect();

                if t[0] == "ON" && t[1] == "-" {
                    break;
                }

                println!("... {} -> {:?} ...", fmri, t);
            } else if lines.len() > 1 {
                bail!("unexpected output for {}: {:?}", fmri, lines);
            }
        }

        sleep(1);
    }

    Ok(())
}

pub fn zone_mount<S1>(name: S1)
    -> Result<()>
    where
        S1: AsRef<str>,
{
    let n = name.as_ref();

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONEADM)
        .arg("-z")
        .arg(n)
        .arg("mount")
        .output()?;

    if !out.status.success() {
        bail!("zoneadm mount {} failure: {}", n, out.info());
    }

    Ok(())
}

pub fn zone_unmount<S1>(name: S1)
    -> Result<()>
    where
        S1: AsRef<str>,
{
    let n = name.as_ref();

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONEADM)
        .arg("-z")
        .arg(n)
        .arg("unmount")
        .output()?;

    if !out.status.success() {
        bail!("zoneadm unmount {} failure: {}", n, out.info());
    }

    Ok(())
}

pub fn zone_uninstall<S1>(name: S1)
    -> Result<()>
    where
        S1: AsRef<str>,
{
    let n = name.as_ref();

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONEADM)
        .arg("-z")
        .arg(n)
        .arg("uninstall")
        .arg("-F")
        .output()?;

    if !out.status.success() {
        bail!("zoneadm uninstall {} failure: {}", n, out.info());
    }

    Ok(())
}

pub fn zone_delete<S1>(name: S1)
    -> Result<()>
    where
        S1: AsRef<str>,
{
    let n = name.as_ref();

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZONECFG)
        .arg("-z")
        .arg(n)
        .arg("delete")
        .arg("-F")
        .output()?;

    if !out.status.success() {
        bail!("zonecfg delete {} failure: {}", n, out.info());
    }

    Ok(())
}

pub fn zone_deposit_script<S1, S2>(name: S1, contents: S2)
    -> Result<String>
    where
        S1: AsRef<str>,
        S2: AsRef<str>,
{
    let n = name.as_ref();
    let c = contents.as_ref();
    let sp = format!("/tmp/helios.build.{}.sh", std::process::id());

    let mut child = Command::new(PFEXEC)
        .env_clear()
        .arg(ZLOGIN)
        .arg("-S")
        .arg(n)
        .arg("tee")
        .arg(&sp)
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(c.as_bytes())?;
        stdin.flush()?;
    }

    let status = child.wait()?;

    if !status.success() {
        bail!("zlogin {} tee {} failure", n, sp);
    }

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZLOGIN)
        .arg("-S")
        .arg(n)
        .arg("/bin/chmod")
        .arg("0755")
        .arg(&sp)
        .output()?;

    if !out.status.success() {
        bail!("zlogin {} chmod {} failure: {}", n, sp, out.info());
    }

    Ok(sp)
}

pub fn zoneinstall_append<S1, S2, P>(name: S1, path: P, line: S2)
    -> Result<()>
    where
        P: AsRef<Path>,
        S1: AsRef<str>,
        S2: AsRef<str>,
{
    let n = name.as_ref();
    let p = path.as_ref();
    let l = line.as_ref();

    let mut child = Command::new(PFEXEC)
        .env_clear()
        .arg(ZLOGIN)
        .arg("-S")
        .arg(n)
        .arg("tee")
        .arg("-a")
        .arg(&p)
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(l.as_bytes())?;
        stdin.flush()?;
    }

    let status = child.wait()?;

    if !status.success() {
        bail!("zlogin {} tee {} failure", n, p.display());
    }

    Ok(())
}

pub fn zoneinstall_mkdir<S1, P>(name: S1, path: P, uid: u32, gid: u32)
    -> Result<()>
    where
        P: AsRef<Path>,
        S1: AsRef<str>,
{
    let n = name.as_ref();
    let p = path.as_ref();

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZLOGIN)
        .arg("-S")
        .arg(n)
        .arg("mkdir")
        .arg("-p")
        .arg(&p)
        .output()?;

    if !out.status.success() {
        bail!("zlogin {} mkdir {} failure: {}", n, p.display(), out.info());
    }

    let out = Command::new(PFEXEC)
        .env_clear()
        .arg(ZLOGIN)
        .arg("-S")
        .arg(n)
        .arg("chown")
        .arg(&format!("{}:{}", uid, gid))
        .arg(&p)
        .output()?;

    if !out.status.success() {
        bail!("zlogin {} chown {} failure: {}", n, p.display(), out.info());
    }

    Ok(())
}
