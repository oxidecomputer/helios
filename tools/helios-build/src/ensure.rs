/*
 * Copyright 2024 Oxide Computer Company
 */

use anyhow::{anyhow, bail, Result};
use slog::{error, info, warn, Logger};
use std::ffi::CString;
use std::ffi::OsStr;
use std::fs::{DirBuilder, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, PartialEq)]
pub enum FileType {
    Directory,
    File,
    Link,
}

#[derive(Debug, PartialEq)]
pub enum Id {
    Name(String),
    Id(u32),
}

#[derive(Debug, PartialEq)]
pub struct FileInfo {
    pub filetype: FileType,
    pub perms: u32,
    pub target: Option<PathBuf>, /* for symbolic links */
}

impl FileInfo {
    pub fn is_user_executable(&self) -> bool {
        (self.perms & libc::S_IXUSR) != 0
    }
}

pub fn check<P: AsRef<Path>>(p: P) -> Result<Option<FileInfo>> {
    let name: &str = p.as_ref().to_str().unwrap();
    let cname = CString::new(name.to_string())?;
    let st =
        Box::into_raw(Box::new(unsafe { std::mem::zeroed::<libc::stat>() }));
    let (r, e, st) = unsafe {
        let r = libc::lstat(cname.as_ptr(), st);
        let e = *libc::___errno();
        (r, e, Box::from_raw(st))
    };
    if r != 0 {
        if e == libc::ENOENT {
            return Ok(None);
        }

        bail!("lstat({}): errno {}", name, e);
    }

    let fmt = st.st_mode & libc::S_IFMT;

    let filetype = if fmt == libc::S_IFDIR {
        FileType::Directory
    } else if fmt == libc::S_IFREG {
        FileType::File
    } else if fmt == libc::S_IFLNK {
        FileType::Link
    } else {
        bail!("lstat({}): unexpected file type: {:x}", name, fmt);
    };

    let target = if filetype == FileType::Link {
        Some(std::fs::read_link(p)?)
    } else {
        None
    };

    let perms = st.st_mode & 0o7777; /* as per mknod(2) */

    Ok(Some(FileInfo { filetype, perms, target }))
}

pub fn perms<P: AsRef<Path>>(log: &Logger, p: P, perms: u32) -> Result<bool> {
    let p = p.as_ref();
    let log = log.new(slog::o!("path" => p.display().to_string()));
    let mut did_work = false;

    let fi = if let Some(fi) = check(p)? {
        fi
    } else {
        bail!("{} does not exist", p.display());
    };

    /*
     * Check permissions on the path.  Note that symbolic links do
     * not actually have permissions, so we skip those completely.
     */
    if fi.filetype != FileType::Link && fi.perms != perms {
        did_work = true;
        info!(log, "perms are {:o}, should be {:o}", fi.perms, perms);

        let cname = CString::new(p.to_str().unwrap().to_string())?;
        let (r, e) = unsafe {
            let r = libc::chmod(cname.as_ptr(), perms);
            let e = *libc::___errno();
            (r, e)
        };
        if r != 0 {
            bail!("lchmod({}, {:o}): errno {}", p.display(), perms, e);
        }

        info!(log, "chmod ok");
    }

    Ok(did_work)
}

pub fn directory<P: AsRef<Path>>(
    log: &Logger,
    dir: P,
    mode: u32,
) -> Result<bool> {
    let dir = dir.as_ref();
    let mut did_work = false;

    if let Some(fi) = check(dir)? {
        /*
         * The path exists already.  Make sure it is a directory.
         */
        if fi.filetype != FileType::Directory {
            bail!("{} is {:?}, not a directory", dir.display(), fi.filetype);
        }
    } else {
        /*
         * Create the directory, and all missing parents:
         */
        did_work = true;
        info!(log, "creating directory: {}", dir.display());
        DirBuilder::new().recursive(true).mode(mode).create(dir)?;

        /*
         * Check the path again, to make sure we have up-to-date information:
         */
        check(dir)?.expect("directory should now exist");
    }

    if perms(log, dir, mode)? {
        did_work = true;
    }

    Ok(did_work)
}

#[derive(Debug, PartialEq)]
pub enum Create {
    IfMissing,
    Always,
}

fn open<P: AsRef<Path>>(p: P) -> Result<File> {
    let p = p.as_ref();

    match File::open(p) {
        Ok(f) => Ok(f),
        Err(e) => Err(anyhow!("opening \"{}\": {}", p.display(), e)),
    }
}

fn comparestr<P: AsRef<Path>>(src: &str, dst: P) -> Result<bool> {
    let dstf = open(dst)?;
    let mut dstr = BufReader::new(dstf);

    /*
     * Assume that if the file can be passed in as a string slice, it can also
     * be loaded into memory fully for comparison.
     */
    let mut dstbuf = Vec::<u8>::new();
    dstr.read_to_end(&mut dstbuf)?;

    Ok(dstbuf == src.as_bytes())
}

fn compare<P1: AsRef<Path>, P2: AsRef<Path>>(src: P1, dst: P2) -> Result<bool> {
    let srcf = open(src)?;
    let dstf = open(dst)?;
    let mut srcr = BufReader::new(srcf);
    let mut dstr = BufReader::new(dstf);

    loop {
        let mut srcbuf = [0u8; 1];
        let mut dstbuf = [0u8; 1];
        let srcsz = srcr.read(&mut srcbuf)?;
        let dstsz = dstr.read(&mut dstbuf)?;

        if srcsz != dstsz {
            /*
             * Files are not the same size...
             */
            return Ok(false);
        }

        if srcsz == 0 {
            /*
             * End-of-file reached, without a mismatched comparison.  These
             * files are equal in contents.
             */
            return Ok(true);
        }

        if srcbuf != dstbuf {
            /*
             * This portion of the read files are not the same.
             */
            return Ok(false);
        }
    }
}

pub fn removed<P: AsRef<Path>>(log: &Logger, dst: P) -> Result<()> {
    let dst = dst.as_ref();

    if let Some(fi) = check(dst)? {
        match fi.filetype {
            FileType::File | FileType::Link => {
                info!(
                    log,
                    "file {} exists (as {:?}), removing",
                    dst.display(),
                    fi.filetype
                );

                std::fs::remove_file(dst)?;
            }
            t => {
                bail!(
                    "file {} exists as {:?}, unexpected type",
                    dst.display(),
                    t
                );
            }
        }
    } else {
        info!(
            log,
            "file {} does not already exist, skipping removal",
            dst.display()
        );
    }

    Ok(())
}

pub fn file_str<P: AsRef<Path>>(
    log: &Logger,
    contents: &str,
    dst: P,
    mode: u32,
    create: Create,
) -> Result<bool> {
    let dst = dst.as_ref();
    let mut did_work = false;

    let do_copy = if let Some(fi) = check(dst)? {
        /*
         * The path exists already.
         */
        match create {
            Create::IfMissing if fi.filetype == FileType::File => {
                info!(
                    log,
                    "file {} exists, skipping population",
                    dst.display()
                );
                false
            }
            Create::IfMissing if fi.filetype == FileType::Link => {
                warn!(
                    log,
                    "symlink {} exists, skipping population",
                    dst.display()
                );
                false
            }
            Create::IfMissing => {
                /*
                 * Avoid clobbering an unexpected entry when we have been asked
                 * to preserve in the face of modifications.
                 */
                bail!(
                    "{} should be a file, but is a {:?}",
                    dst.display(),
                    fi.filetype
                );
            }
            Create::Always if fi.filetype == FileType::File => {
                /*
                 * Check the contents of the file to make sure it matches
                 * what we expect.
                 */
                if comparestr(contents, dst)? {
                    info!(
                        log,
                        "file {} exists, with correct contents",
                        dst.display()
                    );
                    false
                } else {
                    warn!(
                        log,
                        "file {} exists, with wrong contents, unlinking",
                        dst.display()
                    );
                    std::fs::remove_file(dst)?;
                    true
                }
            }
            Create::Always => {
                /*
                 * We found a file type we don't expect.  Try to unlink it
                 * anyway.
                 */
                warn!(
                    log,
                    "file {} exists, of type {:?}, unlinking",
                    dst.display(),
                    fi.filetype
                );
                std::fs::remove_file(dst)?;
                true
            }
        }
    } else {
        info!(log, "file {} does not exist", dst.display());
        true
    };

    if do_copy {
        did_work = true;
        info!(log, "writing {} ...", dst.display());

        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&dst)?;
        f.write_all(contents.as_bytes())?;
        f.flush()?;
    }

    if perms(log, dst, mode)? {
        did_work = true;
    }

    info!(log, "ok!");
    Ok(did_work)
}

pub fn file<P1: AsRef<Path>, P2: AsRef<Path>>(
    log: &Logger,
    src: P1,
    dst: P2,
    mode: u32,
    create: Create,
) -> Result<bool> {
    let src = src.as_ref();
    let dst = dst.as_ref();
    let mut did_work = false;

    let do_copy = if let Some(fi) = check(dst)? {
        /*
         * The path exists already.
         */
        match create {
            Create::IfMissing if fi.filetype == FileType::File => {
                info!(
                    log,
                    "file {} exists, skipping population",
                    dst.display()
                );
                false
            }
            Create::IfMissing if fi.filetype == FileType::Link => {
                warn!(
                    log,
                    "symlink {} exists, skipping population",
                    dst.display()
                );
                false
            }
            Create::IfMissing => {
                /*
                 * Avoid clobbering an unexpected entry when we have been asked
                 * to preserve in the face of modifications.
                 */
                bail!(
                    "{} should be a file, but is a {:?}",
                    dst.display(),
                    fi.filetype
                );
            }
            Create::Always if fi.filetype == FileType::File => {
                /*
                 * Check the contents of the file to make sure it matches
                 * what we expect.
                 */
                if compare(src, dst)? {
                    info!(
                        log,
                        "file {} exists, with correct contents",
                        dst.display()
                    );
                    false
                } else {
                    warn!(
                        log,
                        "file {} exists, with wrong contents, unlinking",
                        dst.display()
                    );
                    std::fs::remove_file(dst)?;
                    true
                }
            }
            Create::Always => {
                /*
                 * We found a file type we don't expect.  Try to unlink it
                 * anyway.
                 */
                warn!(
                    log,
                    "file {} exists, of type {:?}, unlinking",
                    dst.display(),
                    fi.filetype
                );
                std::fs::remove_file(dst)?;
                true
            }
        }
    } else {
        info!(log, "file {} does not exist", dst.display());
        true
    };

    if do_copy {
        did_work = true;
        info!(log, "copying {} -> {} ...", src.display(), dst.display());
        std::fs::copy(src, dst)?;
    }

    if perms(log, dst, mode)? {
        did_work = true;
    }

    info!(log, "ok!");
    Ok(did_work)
}

pub fn symlink<P1: AsRef<Path>, P2: AsRef<Path>>(
    log: &Logger,
    dst: P1,
    target: P2,
) -> Result<bool> {
    let dst = dst.as_ref();
    let target = target.as_ref();
    let mut did_work = false;

    let do_link = if let Some(fi) = check(dst)? {
        if fi.filetype == FileType::Link {
            let fitarget = fi.target.unwrap();
            if fitarget == target {
                info!(log, "link target ok ({})", target.display());
                false
            } else {
                warn!(
                    log,
                    "link target wrong: want {}, got {}; unlinking",
                    target.display(),
                    fitarget.display()
                );
                std::fs::remove_file(dst)?;
                true
            }
        } else {
            /*
             * File type not correct.  Unlink.
             */
            warn!(
                log,
                "file {} exists, of type {:?}, unlinking",
                dst.display(),
                fi.filetype
            );
            std::fs::remove_file(dst)?;
            true
        }
    } else {
        info!(log, "link {} does not exist", dst.display());
        true
    };

    if do_link {
        did_work = true;
        info!(log, "linking {} -> {} ...", dst.display(), target.display());
        std::os::unix::fs::symlink(target, dst)?;
    }

    if perms(log, dst, 0)? {
        did_work = true;
    }

    info!(log, "ok!");
    Ok(did_work)
}

fn spawn_reader<T>(
    log: &Logger,
    name: &str,
    stream: Option<T>,
) -> Option<std::thread::JoinHandle<()>>
where
    T: Read + Send + 'static,
{
    let name = name.to_string();
    let stream = match stream {
        Some(stream) => stream,
        None => return None,
    };

    let log = log.clone();

    Some(std::thread::spawn(move || {
        let mut r = BufReader::new(stream);

        loop {
            let mut buf: Vec<u8> = Vec::new();

            /*
             * We have no particular control over the output from the child
             * processes we run, so we read until a newline character without
             * relying on totally valid UTF-8 output.
             */
            match r.read_until(b'\n', &mut buf) {
                Ok(0) => {
                    /*
                     * EOF.
                     */
                    return;
                }
                Ok(_) => {
                    let s = String::from_utf8_lossy(&buf);
                    let s = s.trim();

                    if !s.is_empty() {
                        info!(log, "{}| {}", name, s);
                    }
                }
                Err(e) => {
                    error!(log, "failed to read {}: {}", name, e);
                    std::process::exit(100);
                }
            }
        }
    }))
}

pub fn run2(log: &Logger, cmd: &mut Command) -> Result<()> {
    let mut logargs = vec![cmd.get_program().to_owned()];
    for arg in cmd.get_args() {
        logargs.push(arg.to_owned());
    }
    info!(log, "exec: {:?}", &logargs; "pwd" => ?cmd.get_current_dir());

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    let readout = spawn_reader(log, "O", child.stdout.take());
    let readerr = spawn_reader(log, "E", child.stderr.take());

    if let Some(t) = readout {
        t.join().expect("join stdout thread");
    }
    if let Some(t) = readerr {
        t.join().expect("join stderr thread");
    }

    match child.wait() {
        Err(e) => Err(e.into()),
        Ok(es) => {
            if !es.success() {
                Err(anyhow!("exec {:?}: failed {:?}", &logargs, &es))
            } else {
                Ok(())
            }
        }
    }
}

fn run_common(log: &Logger, cmd: &mut Command, args: &[&OsStr]) -> Result<()> {
    info!(log, "exec: {:?}", &args; "pwd" => ?cmd.get_current_dir());

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    let readout = spawn_reader(log, "O", child.stdout.take());
    let readerr = spawn_reader(log, "E", child.stderr.take());

    if let Some(t) = readout {
        t.join().expect("join stdout thread");
    }
    if let Some(t) = readerr {
        t.join().expect("join stderr thread");
    }

    match child.wait() {
        Err(e) => Err(e.into()),
        Ok(es) => {
            if !es.success() {
                Err(anyhow!("exec {:?}: failed {:?}", &args, &es))
            } else {
                Ok(())
            }
        }
    }
}

pub fn scrub_env(cmd: &mut Command, utf8: bool) {
    if utf8 {
        cmd.env("LANG", "en_US.UTF-8");
    } else {
        cmd.env_remove("LANG");
    }
    cmd.env_remove("LC_CTYPE");
    cmd.env_remove("LC_NUMERIC");
    cmd.env_remove("LC_TIME");
    cmd.env_remove("LC_COLLATE");
    cmd.env_remove("LC_MONETARY");
    cmd.env_remove("LC_MESSAGES");
    cmd.env_remove("LC_ALL");
}

pub fn run_in<S: AsRef<OsStr>, P: AsRef<Path>>(
    log: &Logger,
    pwd: P,
    args: &[S],
) -> Result<()> {
    let args: Vec<&OsStr> = args.iter().map(|s| s.as_ref()).collect();

    let mut cmd = Command::new(&args[0]);
    cmd.current_dir(pwd.as_ref());

    scrub_env(&mut cmd, false);

    if args.len() > 1 {
        cmd.args(&args[1..]);
    }

    run_common(log, &mut cmd, args.as_slice())
}

pub fn run<S: AsRef<OsStr>>(log: &Logger, args: &[S]) -> Result<()> {
    let args: Vec<&OsStr> = args.iter().map(|s| s.as_ref()).collect();

    let mut cmd = Command::new(&args[0]);

    scrub_env(&mut cmd, false);

    if args.len() > 1 {
        cmd.args(&args[1..]);
    }

    run_common(log, &mut cmd, args.as_slice())
}

pub fn run_utf8<S: AsRef<OsStr>>(log: &Logger, args: &[S]) -> Result<()> {
    let args: Vec<&OsStr> = args.iter().map(|s| s.as_ref()).collect();

    let mut cmd = Command::new(&args[0]);

    scrub_env(&mut cmd, true);

    if args.len() > 1 {
        cmd.args(&args[1..]);
    }

    run_common(log, &mut cmd, args.as_slice())
}

pub fn run_env<S, K, V, I>(log: &Logger, args: &[S], env: I) -> Result<()>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let args: Vec<&OsStr> = args.iter().map(|s| s.as_ref()).collect();

    let mut cmd = Command::new(&args[0]);

    cmd.env_clear();
    cmd.envs(env);

    if args.len() > 1 {
        cmd.args(&args[1..]);
    }

    run_common(log, &mut cmd, args.as_slice())
}
