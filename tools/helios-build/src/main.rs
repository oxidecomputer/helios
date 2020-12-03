mod common;
use common::*;

pub mod illumos;
pub mod ensure;

use anyhow::{Result, Context, bail, anyhow};
use serde::{Serialize, Deserialize};
use std::collections::{BTreeMap, HashMap, VecDeque, BTreeSet};
use std::process::Command;
use std::os::unix::process::CommandExt;
use std::io::{BufWriter, BufReader, Write, Read};
use std::fs::File;
use slog::Logger;
use illumos::ZonesExt;
use std::path::Path;
use walkdir::{WalkDir, DirEntry};
use regex::Regex;

const PKGREPO: &str = "/usr/bin/pkgrepo";

const RELVER: u32 = 1;
const DASHREV: u32 = 0;

fn baseopts() -> getopts::Options {
    let mut opts = getopts::Options::new();

    /*
     * We should always have a --help flag everywhere.
     */
    opts.optflag("", "help", "usage information");

    opts
}

use std::path::{PathBuf, Component};
use std::ffi::OsStr;

fn pc(s: &str) -> Component {
    Component::Normal(OsStr::new(s))
}

/**
 * Determine the location of the top-level helios.git clone.
 */
fn top() -> Result<PathBuf> {
    /*
     * Start with the path of the current executable, and discard the last
     * component of that path (the executable file itself) so that we can reason
     * about the directory structure in which it is found.
     */
    let mut exe = std::env::current_exe()?;
    let last: Vec<Component> = exe.components().rev().skip(1).collect();

    if last.len() < 4 {
        bail!("could not determine path from {:?}", last);
    }

    if (last[0] != pc("debug") && last[0] != pc("release"))
        || last[1] != pc("target")
        || last[2] != pc("helios-build")
        || last[3] != pc("tools")
    {
        bail!("could not determine path from {:?}", last);
    }

    for _ in 0..5 {
        assert!(exe.pop());
    }

    Ok(exe)
}

fn top_path(components: &[&str]) -> Result<PathBuf> {
    let mut top = top()?;
    for c in components {
        top.push(c);
    }
    Ok(top)
}

#[derive(Debug, Deserialize)]
struct Projects {
    #[serde(default)]
    project: HashMap<String, Project>,
}

#[derive(Debug, Deserialize)]
struct Project {
    github: Option<String>,
    url: Option<String>,

    /*
     * If this is a private repository, we force the use of SSH:
     */
    #[serde(default)]
    use_ssh: bool,

    /*
     * Set up an OmniOS-style lib/site.sh for this project:
     */
    #[serde(default)]
    site_sh: bool,
}

impl Project {
    fn url(&self, use_ssh: bool) -> Result<String> {
        if let Some(url) = self.url.as_deref() {
            Ok(url.to_string())
        } else if let Some(github) = self.github.as_deref() {
            Ok(if use_ssh || self.use_ssh {
                format!("git@github.com:{}.git", github)
            } else {
                format!("https://github.com/{}.git", github)
            })
        } else {
            bail!("need github or url?");
        }
    }
}

fn ensure_dir(components: &[&str]) -> Result<PathBuf> {
    let dir = top_path(components)?;
    if !exists_dir(&dir)? {
        std::fs::create_dir(&dir)?;
    }
    Ok(dir)
}


#[derive(Debug, Deserialize)]
struct UserlandMetadata {
    dependencies: Vec<String>,
    fmris: Vec<String>,
    name: String,
}

fn create_ips_repo<P, S>(log: &Logger, path: P, publ: S, torch: bool)
    -> Result<()>
    where P: AsRef<Path>,
          S: AsRef<str>,
{
    let publ: &str = publ.as_ref();
    let path: &Path = path.as_ref();
    let paths = path.to_str().unwrap();

    if exists_dir(path)? {
        if !torch {
            info!(log, "repository {} exists, skipping creation", paths);
            return Ok(());
        }
        info!(log, "repository {} exists, removing first", paths);
        std::fs::remove_dir_all(&path)?;
    }

    ensure::run(log, &[PKGREPO, "create", paths])?;
    ensure::run(log, &[PKGREPO, "add-publisher", "-s", paths, publ])?;

    info!(log, "repository {} for publisher {} created", paths, publ);

    Ok(())
}

fn cmd_promote_illumos(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] promote-illumos [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    let publisher = "on-nightly";
    let staging = top_path(&["projects", "illumos", "packages", "i386",
        "merged"])?;
    let repo_d = top_path(&["projects", "illumos", "packages", "i386",
        "nightly", "repo.redist"])?;
    let repo_nd = top_path(&["projects", "illumos", "packages", "i386",
        "nightly-nd", "repo.redist"])?;

    /*
     * Merge the packages from the DEBUG and non-DEBUG builds into a single
     * staging repository using the IPS variant feature.
     */
    info!(log, "recreating staging repository at {:?}", &staging);
    create_ips_repo(log, &staging, &publisher, true)?;

    ensure::run(log, &["/usr/bin/pkgmerge", "-d", &staging.to_str().unwrap(),
        "-s", &format!("debug.illumos=false,{}/", repo_nd.to_str().unwrap()),
        "-s", &format!("debug.illumos=true,{}/", repo_d.to_str().unwrap())])?;

    Ok(())
}

fn ncpus() -> Result<u32> {
    /*
     * XXX Replace with kstat check.
     */
    /* "psrinfo -t" */
    let out = Command::new("/usr/sbin/psrinfo")
        .env_clear()
        .arg("-t")
        .output()?;

    if !out.status.success() {
        bail!("could not count CPUs: {}", out.info());
    }

    let stdout = String::from_utf8(out.stdout)?;
    Ok(stdout.trim().parse().context("psrinfo parse failure")?)
}

fn regen_illumos_sh(log: &Logger) -> Result<()> {
    let gate = top_path(&["projects", "illumos"])?;
    let path_env = top_path(&["projects", "illumos", "illumos.sh"])?;

    let maxjobs = ncpus()?;

    /*
     * Though git does not support an SVN- or Mercurial-like revision number,
     * our history is sufficiently linear that we can approximate one anyway.
     * Use that to set an additional version number component beyond the
     * release version:
     */
    let rnum = git_commit_count(&gate)?;

    /*
     * Construct an environment file to build illumos-gate.
     */
    let mut env = String::new();
    env += "export NIGHTLY_OPTIONS='-nCDAmprt'\n";
    env += &format!("export CODEMGR_WS='{}'\n", gate.to_str().unwrap());
    env += "export GNUC_ROOT=/opt/gcc-7\n";
    env += "export PRIMARY_CC=gcc7,$GNUC_ROOT/bin/gcc,gnu\n";
    env += "export PRIMARY_CCC=gcc7,$GNUC_ROOT/bin/g++,gnu\n";
    //env += "export SHADOW_CCS=gcc4,$GNUC_ROOT/bin/gcc,gnu\n";
    //env += "export SHADOW_CCS=gcc4,$GNUC_ROOT/bin/g++,gnu\n";
    //env += "export ENABLE_SMB_PRINTING=
    env += "export BUILDVERSION_EXEC=\"git describe --all --long --dirty\"\n";
    env += &format!("export DMAKE_MAX_JOBS={}\n", maxjobs);
    env += "export ENABLE_SMB_PRINTING='#'\n";
    env += "export PERL_VERSION=5.32\n";
    env += "export PERL_PKGVERS=\n";
    env += "export PERL_VARIANT=-thread-multi\n";
    env += "export BUILDPERL32='#'\n";
    env += "export JAVA_ROOT=/usr/jdk/openjdk11.0\n";
    env += "export JAVA_HOME=$JAVA_ROOT\n";
    env += "export BLD_JAVA_11=\n";
    env += "export BUILDPY2=\n";
    env += "export BUILDPY3=\n";
    env += "export BUILDPY2TOOLS=\n";
    env += "export BUILDPY3TOOLS=\n";
    env += "export PYTHON3_VERSION=3.7\n";
    env += "export PYTHON3_PKGVERS=-37\n";
    env += "export TOOLS_PYTHON=/usr/bin/python$PYTHON3_VERSION\n";
    env += "export STAFFER=\"$LOGNAME\"\n";
    env += "export MAILTO=\"${MAILTO:-$STAFFER}\"\n";
    env += "export BUILD_PROJECT=''\n";
    env += "export ATLOG=\"$CODEMGR_WS/log\"\n";
    env += "export LOGFILE=\"$ATLOG/nightly.log\"\n";
    env += "export MACH=\"$(uname -p)\"\n";
    env += "export BUILD_TOOLS='/opt'\n";
    env += "export MAKEFLAGS='k'\n";
    env += "export PARENT_WS=''\n";
    env += "export REF_PROTO_LIST=\"$PARENT_WS/usr/src/proto_list_${MACH}\"\n";
    env += "export PARENT_ROOT=\"$PARENT_WS/proto/root_$MACH\"\n";
    env += "export PARENT_TOOLS_ROOT=\
        \"$PARENT_WS/usr/src/tools/proto/root_$MACH-nd\"\n";
    env += "export PKGARCHIVE=\"${CODEMGR_WS}/packages/${MACH}/nightly\"\n";
    env += "export VERSION=\"`git describe --long --all HEAD \
        | cut -d/ -f2-`\"\n";
    env += "export ROOT=\"$CODEMGR_WS/proto/root_${MACH}\"\n";
    env += "export SRC=\"$CODEMGR_WS/usr/src\"\n";
    env += "export MULTI_PROTO=\"yes\"\n";
    env += "export ONBLD_BIN=/opt/onbld/bin\n";
    env += "export ON_CLOSED_BINS=/opt/onbld/closed\n";
    env += &format!("export PKGVERS_BRANCH={}.{}.{}\n", RELVER, DASHREV, rnum);

    ensure::file_str(log, &env, &path_env, 0o644, ensure::Create::Always)?;

    Ok(())
}

fn cmd_build_illumos(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] build-illumos [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    regen_illumos_sh(log)?;

    let gate = top_path(&["projects", "illumos"])?;

    let script = format!("cd {} && ./usr/src/tools/scripts/nightly illumos.sh",
        gate.to_str().unwrap());

    ensure::run(log, &["/sbin/sh", "-c", &script])?;

    Ok(())
}

fn cmd_illumos_bldenv(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] bldenv [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    regen_illumos_sh(log)?;

    let env = top_path(&["projects", "illumos", "illumos.sh"])?;
    let src = top_path(&["projects", "illumos", "usr", "src"])?;
    let bldenv = top_path(&["projects", "illumos", "usr", "src",
        "tools", "scripts", "bldenv"])?;

    /*
     * bldenv(1) starts an interactive build shell with the correct environment
     * for running dmake(1) and other illumos build tools.  As such, we want to
     * exec(2) and replace this process rather than run it as a logged child
     * process.
     */
    let err = Command::new(&bldenv)
        .arg(&env)
        .current_dir(&src)
        .exec();
    bail!("exec failure: {:?}", err);
}

#[derive(Debug)]
enum BuildFile {
    Script(PathBuf),
    Manifest(PathBuf),
}

struct BuildPackage {
    name: String,
    file: BuildFile,
}

fn read_string(path: &Path) -> Result<String> {
    let f = File::open(path)?;
    let mut buf = String::new();
    let mut br = BufReader::new(&f);
    br.read_to_string(&mut buf)?;
    Ok(buf)
}

fn extract_pkgs(log: &Logger, dir: &Path) -> Result<Vec<BuildPackage>> {
    /*
     * First, find all the build.sh scripts.
     */
    fn is_build_sh(ent: &DirEntry) -> bool {
        ent.file_type().is_file() &&
            ent.file_name().to_str()
            .map(|s| s.starts_with("build") && s.ends_with(".sh"))
            .unwrap_or(false)
    }

    fn is_p5m(ent: &DirEntry) -> bool {
        ent.file_type().is_file() &&
            ent.file_name().to_str()
            .map(|s| s.ends_with(".p5m"))
            .unwrap_or(false)
    }

    let mut out = Vec::new();
    let re = Regex::new(r"\bPKG=([^[:space:]]+)[[:space:]]*(#.*)?$").unwrap();
    let re2 = Regex::new(r"^set name=pkg.fmri value=([^[:space:]]+).*")
        .unwrap();
    let re3 = Regex::new("^(?:.*//[^/]*/)?(.+?)(?:@.*)$").unwrap();

    for ent in WalkDir::new(&dir).into_iter() {
        let ent = ent?;

        if is_p5m(&ent) {
            for l in read_string(&ent.path())?.lines() {
                if let Some(cap) = re2.captures(&l) {
                    let pkg = cap.get(1).unwrap().as_str();
                    if let Some(cap) = re3.captures(&pkg) {
                        let pkg = cap.get(1).unwrap().as_str();
                        out.push(BuildPackage {
                            name: pkg.to_string(),
                            file: BuildFile::Manifest(ent.path().to_path_buf()),
                        });
                    } else {
                        bail!("weird package? {}", l);
                    }
                }
            }
            continue;
        }

        if !is_build_sh(&ent) {
            continue;
        }

        /*
         * Inspect the contents of each build script and look for packages.
         */
        for l in read_string(&ent.path())?.lines() {
            if l.contains("##IGNORE##") {
                continue;
            }

            if let Some(cap) = re.captures(&l) {
                if let Some(pkg) = cap.get(1) {
                    let pkg = pkg.as_str().trim();
                    if !pkg.is_empty() {
                        out.push(BuildPackage {
                            name: pkg.to_string(),
                            file: BuildFile::Script(ent.path().to_path_buf()),
                        });
                    }
                }
            }
        }
    }

    Ok(out)
}

fn cmd_build_omnios(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] build-omnios \
            [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    // if res.free.is_empty() {
    //     bail!("which package should I build?");
    // }

    let dir = top_path(&["projects", "omnios-build", "build"])?;

    let mut pkgs = extract_pkgs(log, &dir)?;

    pkgs.sort_by(|a, b| a.name.cmp(&b.name));

    for pkg in pkgs.iter() {
        println!(" * {}", pkg.name);
        println!("   {:?}", pkg.file);
    }

    Ok(())
}

fn cmd_build(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] build [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if res.free.is_empty() {
        bail!("which package should I build?");
    } else if res.free.len() > 1 {
        bail!("only one package build at a time right now");
    }
    let target = &res.free[0];

    build(log, target)?;
    Ok(())
}

fn userland_gmake<P: AsRef<Path>>(log: &Logger, targetdir: P, target: &str)
    -> Result<()>
{
    let targetdir = targetdir.as_ref();
    let archive = top_path(&["cache", "userland-archive"])?;

    ensure::run_env(log, &[
        "gmake", "-s", "-C", &targetdir.to_str().unwrap(), target
    ], vec![
        ("USERLAND_ARCHIVES", format!("{}/", archive.to_str().unwrap()))
    ])?;

    Ok(())
}

fn build(log: &Logger, target: &str) -> Result<()> {
    info!(log, "BUILD: {}", target);

    let zones = illumos::zone_list()?;
    if !zones.exists("helios-template") {
        bail!("create helios-template zone first");
    }

    /*
     * Tear down any existing zone, to make sure we are not racing with a prior
     * build that may still be running.
     */
    let bzn = "helios-build";
    let bzr = format!("/zones/{}/root", bzn); /* XXX */
    if zones.exists(bzn) {
        let z = zones.by_name(bzn)?;

        /*
         * Destroy the existing zone.
         */
        let (unmount, halt, uninstall, delete) = match z.state.as_str() {
            "mounted" => (true, false, true, true),
            "running" => (false, true, true, true),
            "installed" => (false, false, true, true),
            "incomplete" => (false, false, true, true),
            "configured" => (false, false, false, true),
            n => bail!("unexpected zone state: {}", n),
        };

        if unmount {
            info!(log, "unmounting...");
            illumos::zone_unmount(&z.name)?;
        }
        if halt {
            info!(log, "halting...");
            illumos::zone_halt(&z.name)?;
        }
        if uninstall {
            info!(log, "uninstalling...");
            illumos::zone_uninstall(&z.name)?;
        }
        if delete {
            info!(log, "deleting...");
            illumos::zone_delete(&z.name)?;
        }
    }

    /*
     * Download any required components for this build.
     */
    let targetdir = top_path(&["projects", "userland", "components",
        &target])?;
    userland_gmake(log, &targetdir, "download")?;

    /*
     * Make sure the metadata is up-to-date for this component.
     */
    let umd = read_metadata(log, &target)?;

    info!(log, "creating...");
    illumos::zone_create(bzn, format!("/zones/{}", bzn), "nlipkg")?;

    let top = top()?;
    println!("helios repository root is: {}", top.display());

    info!(log, "adding lofs...");
    illumos::zone_add_lofs(bzn, &top, &top)?;

    info!(log, "cloning...");
    illumos::zone_clone(bzn, "helios-template")?;

    /*
     * Before booting the zone, we must make sure that we have installed all of
     * the required packages for this build.
     *
     * First, add our local build repository to the ephemeral zone so that we
     * are preferring packages we may have rebuilt locally.
     */
    let repodir = top_path(&["projects", "userland", "i386", "repo"])?;
/* XXX ugh */
//      ensure::run(log, &["pfexec", "/usr/bin/pkg", "-R", &bzr,
//        "set-publisher",
//        "-g", &format!("file://{}", repodir.to_str().unwrap()),
//        "--sticky",
//        "--search-first",
//        "userland"])?;
//    ensure::run(log, &["pfexec", "/usr/bin/pkg", "-R", &bzr,
//        "set-publisher",
//        "--non-sticky",
//        "openindiana.org"])?;
    ensure::run(log, &["pfexec", "/usr/bin/pkg", "-R", &bzr,
        "uninstall",
        "userland-incorporation",
        "entire"])?;

    let mut install = Vec::new();
    for dep in umd.dependencies.iter() {
        info!(log, "checking for {}...", dep);
        let out = Command::new("pfexec")
            .env_clear()
            .arg("/usr/bin/pkg")
            .arg("-R")
            .arg(&bzr)
            .arg("info")
            .arg("-q")
            .arg(format!("{}", dep))
            .output()?;

        if !out.status.success() {
            install.push(dep);
        }
    }

    if !install.is_empty() {
        info!(log, "installing packages in zone: {:?}", install);
        let mut args = vec!["pfexec", "/usr/bin/pkg", "-R", &bzr, "install"];
        let mut argstorage = Vec::new();
        for i in install.iter() {
            if i.starts_with("pkg:") {
                bail!("not expecting full FMRI: {}", i);
            }
            if i.starts_with("/") {
                argstorage.push(i.to_string());
            } else {
                argstorage.push(format!("/{}", i));
            }
        }
        for arg in argstorage.iter() {
            args.push(arg.as_str());
        }
        ensure::run(log, &args)?;
    }

    /*
     * We want to create a user account in the zone that has the same
     * credentials as the user in the global, so that we don't mess up the file
     * system permissions on the workspace.
     */
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    if uid != 0 {
        info!(log, "uid {} gid {}", uid, gid);

        info!(log, "mounting...");
        illumos::zone_mount(bzn)?;

        /*
         * When mounted, we are able to execute programs in the zone in a safe
         * fashion.  The zone root will be mounted at "/a" in the context we
         * enter here:
         */
        illumos::zoneinstall_mkdir(bzn, "/a/export/home/build", uid, gid)?;

        let passwd = format!("build:x:{}:{}:Build User\
            :/export/home/build:/bin/bash\n", uid, gid);
        let shadow = format!("build:NP:18494::::::\n");
        illumos::zoneinstall_append(bzn, "/a/etc/passwd", passwd)?;
        illumos::zoneinstall_append(bzn, "/a/etc/shadow", shadow)?;

        illumos::zone_unmount(bzn)?;
    }

    info!(log, "booting...");
    illumos::zone_boot(bzn)?;
    illumos::zone_milestone_wait(log, bzn,
        "svc:/milestone/multi-user-server:default")?;

    let archive = top_path(&["cache", "userland-archive"])?;
    let buildscript = format!("#!/bin/bash\n\
        set -o errexit\n\
        set -o pipefail\n\
        set -o xtrace\n\
        export USERLAND_ARCHIVES='{}/'\n\
        export COMPONENT_BUILD_ARGS='-j10'\n\
        cd '{}'\n\
        /usr/bin/gmake publish\n
        /usr/bin/gmake sample-manifest\n",
        archive.to_str().unwrap(),
        targetdir.to_str().unwrap());
    let spath = illumos::zone_deposit_script(bzn, &buildscript)?;
    ensure::run(log, &["pfexec", "zlogin", "-l", "build", bzn, &spath])?;

    info!(log, "ok");
    Ok(())
}

fn cmd_zone(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] zone [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    // let top = top()?;
    // println!("helios repository root is: {}", top.display());

    let zones = illumos::zone_list()?;
    println!("zones: {:#?}", zones);
    let mut install = false;

    if !zones.exists("helios-template") {
        /*
         * Create the template zone!
         */
        illumos::zone_create("helios-template", "/zones/helios-template",
            "nlipkg")?;
        install = true;
    } else {
        let z = zones.by_name("helios-template")?;

        if z.state == "configured" {
            install = true;
        }
    }

    if install {
        println!("installing zone!");
        illumos::zone_install("helios-template", &["build-essential"])?;
    }

    Ok(())
}

fn cmd_archive(log: &Logger, args: &[&str]) -> Result<()> {
    let mut opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] setup [OPTIONS]"));
    };

    let res = opts.parse(args)?;
    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    let datafile = top_path(&["cache", "assets.json"])?;
    let data: BTreeMap<String, Asset> =
        serde_json::from_reader(File::open(&datafile)?)?;

    let mut missing = Vec::new();

    for a in data.values() {
        let p = top_path(&["cache", "userland-archive", &a.file])?;

        if !exists_file(&p)? {
            missing.push(p);
        }
    }

    for m in missing.iter() {
        println!("MISSING: {}", m.display());
    }

    let mut d = std::fs::read_dir(&top_path(&["cache", "userland-archive"])?)?;
    while let Some(ent) = d.next().transpose()? {
        if ent.file_type().unwrap().is_file() {
            let name = ent.file_name().into_string().unwrap();
            if !data.contains_key(&name) {
                println!("SUPERFLUOUS: {}", name);
            }
        }
    }

    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Asset {
    file: String,
    url: String,
    sigurl: Option<String>,
    hash: Option<String>,
    src_dir: String,
}

fn cmd_download_metadata(log: &Logger, args: &[&str]) -> Result<()> {
    let mut opts = baseopts();

    opts.reqopt("", "file", "", "");
    opts.reqopt("", "url", "", "");
    opts.optopt("", "sigurl", "", "");
    opts.optopt("", "hash", "", "");
    opts.reqopt("", "dir", "", "");

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] setup [OPTIONS]"));
    };

    let res = opts.parse(args)?;
    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    let file = res.opt_str("file").unwrap();
    let url = res.opt_str("url").unwrap();
    let hash = res.opt_str("hash");
    let src_dir = res.opt_str("dir").unwrap();
    let sigurl = res.opt_str("sigurl");

    ensure_dir(&["cache"])?;

    let datafile = top_path(&["cache", "assets.json"])?;

    let mut data: BTreeMap<String, Asset> =
        if let Ok(f) = File::open(&datafile) {
            let r = BufReader::new(f);
            serde_json::from_reader(r)?
        } else {
            BTreeMap::new()
        };

    data.insert(file.clone(), Asset {
        file,
        url,
        sigurl,
        hash,
        src_dir,
    });

    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&datafile)?;
    let mut w = BufWriter::new(f);
    serde_json::to_writer_pretty(&mut w, &data)?;
    w.flush()?;

    Ok(())
}

#[derive(Debug, Deserialize)]
struct UserlandMapping {
    fmri: String,
    name: String,
    path: String,
    repo: Option<String>,
}

fn read_metadata(log: &Logger, target: &str) -> Result<UserlandMetadata> {
    let targetdir = top_path(&["projects", "userland", "components",
        &target])?;
    ensure::run_utf8(log, &["/usr/bin/gmake", "-s",
        "-C", targetdir.to_str().unwrap(),
        "update-metadata"])?;
    let mut mdf = targetdir.clone();
    mdf.push("pkg5");
    let f = File::open(&mdf)?;
    Ok(serde_json::from_reader(&f)?)
}

#[derive(Debug, Deserialize)]
struct PkgRepoList {
    branch: String,
    #[serde(rename = "build-release")]
    build_release: String,
    name: String,
    publisher: String,
    release: String,
    timestamp: String,
    version: String,
    #[serde(rename = "pkg.fmri")]
    fmri: String,
}

fn git_commit_count<P: AsRef<Path>>(path: P) -> Result<u32> {
    let out = Command::new("git")
        .env_clear()
        .arg("rev-list")
        .arg("--count")
        .arg("HEAD")
        .current_dir(path.as_ref())
        .output()?;

    if !out.status.success() {
        bail!("git commit count failed: {}", out.info());
    }

    let res = String::from_utf8(out.stdout)?;
    Ok(res.trim().parse()?)
}

fn repo_contains(log: &Logger, fmri: &str) -> Result<bool> {
    let repodir = top_path(&["projects", "userland", "i386", "repo"])?;

    info!(log, "checking build for {}...", fmri);
    let out = Command::new("/usr/bin/pkgrepo")
        .env_clear()
        .arg("list")
        .arg("-F")
        .arg("json-formatted")
        .arg("-s")
        .arg(&repodir.to_str().unwrap())
        .arg(fmri)
        .output()?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.contains("did not match any packages") {
            bail!("pkgrepo list failed: {}", out.info());
        }
    }

    let pkgs: Vec<PkgRepoList> = serde_json::from_slice(&out.stdout)?;

    if pkgs.is_empty() {
        info!(log, "found no versions");
        Ok(false)
    } else {
        for pkg in pkgs.iter() {
            info!(log, "found version {}", pkg.version);
        }
        Ok(true)
    }
}

#[derive(Debug)]
struct ActionDepend {
    fmri: Vec<String>,
    type_: String,
    predicate: Vec<String>,
    variant_zone: Option<String>,
}

impl ActionDepend {
    fn fmris(&self) -> Vec<&str> {
        self.fmri.iter().map(|x| x.as_str()).collect()
    }
}

#[derive(Debug)]
enum Action {
    Depend(ActionDepend),
    Unknown(String, Vec<String>, Vals),
}

#[derive(Debug)]
enum ParseState {
    Rest,
    Type,
    Key,
    Value,
    ValueQuoted,
    ValueQuotedSpace,
    ValueUnquoted,
}

#[derive(Debug)]
struct Vals {
    vals: Vec<(String, String)>,
    extra: BTreeSet<String>,
}

impl Vals {
    fn new() -> Vals {
        Vals {
            vals: Vec::new(),
            extra: BTreeSet::new(),
        }
    }

    fn insert(&mut self, key: &str, value: &str) {
        /*
         * XXX Ignore "facet.*" properties for now...
         */
        if key.starts_with("facet.") {
            return;
        }

        self.vals.push((key.to_string(), value.to_string()));
        self.extra.insert(key.to_string());
    }

    fn maybe_single(&mut self, name: &str) -> Result<Option<String>> {
        let mut out: Option<String> = None;

        for (k, v) in self.vals.iter() {
            if k == name {
                if out.is_some() {
                    bail!("more than one value for {}, wanted a single value",
                        name);
                }
                out = Some(v.to_string());
            }
        }

        self.extra.remove(name);
        Ok(out)
    }

    fn single(&mut self, name: &str) -> Result<String> {
        let out = self.maybe_single(name)?;

        if let Some(out) = out {
            Ok(out)
        } else {
            bail!("no values for {} found", name);
        }
    }

    fn maybe_list(&mut self, name: &str) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();

        for (k, v) in self.vals.iter() {
            if k == name {
                out.push(v.to_string());
            }
        }

        self.extra.remove(name);
        Ok(out)
    }

    fn list(&mut self, name: &str) -> Result<Vec<String>> {
        let out = self.maybe_list(name)?;
        if out.is_empty() {
            bail!("wanted at least one value for {}, found none", name);
        }
        Ok(out)
    }

    fn check_for_extra(&self) -> Result<()> {
        if !self.extra.is_empty() {
            bail!("some properties present but not consumed: {:?}, {:?}",
                self.extra, self.vals);
        }

        Ok(())
    }
}

fn parse_manifest(log: &Logger, input: &str) -> Result<Vec<Action>> {
    let mut out = Vec::new();

    for l in input.lines() {
        let mut s = ParseState::Rest;
        let mut a = String::new();
        let mut k = String::new();
        let mut v = String::new();
        let mut vals = Vals::new();
        let mut free: Vec<String> = Vec::new();
        let mut quote = '"';

        for c in l.chars() {
            match s {
                ParseState::Rest => {
                    if c.is_ascii_alphabetic() {
                        a.clear();
                        k.clear();
                        v.clear();

                        a.push(c);
                        s = ParseState::Type;
                    } else {
                        bail!("invalid line ({:?}): {}", s, l);
                    }
                }
                ParseState::Type => {
                    if c.is_ascii_alphabetic() {
                        a.push(c);
                    } else if c == ' ' {
                        s = ParseState::Key;
                    } else {
                        bail!("invalid line ({:?}): {}", s, l);
                    }
                }
                ParseState::Key => {
                    if c.is_ascii_alphanumeric()
                        || c == '.' || c == '-' || c == '_' || c == '/'
                        || c == '@'
                    {
                        k.push(c);
                    } else if c == ' ' {
                        free.push(k.clone());
                        k.clear();
                    } else if c == '=' {
                        s = ParseState::Value;
                    } else {
                        bail!("invalid line ({:?}, {}): {}", s, k, l);
                    }
                }
                ParseState::Value => {
                    /*
                     * This state represents the start of a new value, which
                     * will either be quoted or unquoted.
                     */
                    v.clear();
                    if c == '"' || c == '\'' {
                        /*
                         * Record the type of quote used at the start of the
                         * string so that we can match it with the same type
                         * of quote at the end.
                         */
                        quote = c;
                        s = ParseState::ValueQuoted;
                    } else {
                        s = ParseState::ValueUnquoted;
                        v.push(c);
                    }
                }
                ParseState::ValueQuoted => {
                    if c == '\\' {
                        /*
                         * XXX handle escaped quotes...
                         */
                        bail!("invalid line (backslash...): {}", l);
                    } else if c == quote {
                        s = ParseState::ValueQuotedSpace;
                    } else {
                        v.push(c);
                    }
                }
                ParseState::ValueQuotedSpace => {
                    /*
                     * We expect at least one space after a quoted string before
                     * the next key.
                     */
                    if c == ' ' {
                        vals.insert(&k, &v);
                        s = ParseState::Key;
                        k.clear();
                    } else {
                        bail!("invalid after quote ({:?}, {}): {}", s, k, l);
                    }
                }
                ParseState::ValueUnquoted => {
                    if c == '"' || c == '\'' {
                        bail!("invalid line (errant quote...): {}", l);
                    } else if c == ' ' {
                        vals.insert(&k, &v);
                        s = ParseState::Key;
                        k.clear();
                    } else {
                        v.push(c);
                    }
                }
            }
        }

        match s {
            ParseState::ValueQuotedSpace | ParseState::ValueUnquoted => {
                vals.insert(&k, &v);
            }
            ParseState::Type => {},
            _ => bail!("invalid line (terminal state {:?}: {}", s, l),
        }

        match a.as_str() {
            "depend" => {
                let fmri = vals.list("fmri")?;
                let type_ = vals.single("type")?;
                let predicate = vals.maybe_list("predicate")?;
                let variant_zone = vals.maybe_single(
                    "variant.opensolaris.zone")?;

                vals.check_for_extra()?;

                out.push(Action::Depend(ActionDepend {
                    fmri,
                    type_,
                    predicate,
                    variant_zone,
                }))
            }
            _ => out.push(Action::Unknown(a.to_string(), free, vals)),
        }
    }

    Ok(out)
}

fn repo_contents(log: &Logger, fmri: &str) -> Result<Vec<Action>> {
    let repodir = top_path(&["projects", "userland", "i386", "repo"])?;

    info!(log, "checking contents for {}...", fmri);
    let out = Command::new("/usr/bin/pkgrepo")
        .env_clear()
        .arg("contents")
        .arg("-m")
        .arg("-s")
        .arg(&repodir.to_str().unwrap())
        .arg(fmri)
        .output()?;

    if !out.status.success() {
        bail!("pkgrepo contents failed: {}", out.info());
    }

    /*
     * Parse the output manifest lines...
     */
    Ok(parse_manifest(log, &String::from_utf8(out.stdout)?)?)
}

fn cmd_userland_promote(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] \
            userland-promote [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    let top = top()?;
    println!("helios repository root is: {}", top.display());

    /*
     * Rebuild the IPS repository:
     */
    let repo = top_path(&["projects", "userland", "i386", "repo"])?;
    ensure::run(log, &["/usr/bin/pkgrepo", "rebuild", "-s",
        &repo.to_str().unwrap()])?;

    /*
     * Generate the userland-incorporation:
     *
     * XXX It seems like this should really be generated as part of a final
     * publish of new packages, as it depends on the full repository contents
     * being available -- but we will really only have the packages we are
     * rebuilding.
     */
    let compdir = top_path(&["projects", "userland", "components"])?;
    userland_gmake(log, &compdir, "incorporation")?;

    ensure::run(log, &["/usr/bin/pkgrepo", "refresh", "-s",
        &repo.to_str().unwrap()])?;

    /*
     * Promote the latest version of each package in the userland dock,
     * transforming the publisher as we go:
     */
    let dock = top_path(&["packages", "repo"])?;
    let transforms = top_path(&["packages", "publisher.mogrify"])?;
    ensure::run(log, &["/usr/bin/pkgrecv",
        "-s", &repo.to_str().unwrap(),
        "-d", &dock.to_str().unwrap(),
        "--mog-file", &transforms.to_str().unwrap(),
        "-m", "latest",
        "-r",
        "-v",
        "*"])?;

    ensure::run(log, &["/usr/bin/pkgrepo", "refresh", "-s",
        &dock.to_str().unwrap()])?;

    Ok(())
}

#[derive(Serialize, Deserialize)]
struct MemoQueueEntry {
    fmri: String,
    optional: bool,
}

#[derive(Serialize, Deserialize)]
struct Memo {
    seen: BTreeSet<String>,
    q: VecDeque<MemoQueueEntry>,
    #[serde(default)]
    fails: VecDeque<MemoQueueEntry>,
}

fn memo_load<T>(log: &Logger, mdf: &str) -> Result<Option<T>>
    where for<'de> T: Deserialize<'de>,
{
    let f = match File::open(&mdf) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("could not load memo file: {:?}", e),
    };
    Ok(serde_json::from_reader(&f)?)
}

fn memo_store<T>(log: &Logger, mdf: &str, t: T) -> Result<()>
    where T: Serialize,
{
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(&mdf)?;
    serde_json::to_writer_pretty(&mut f, &t)?;
    Ok(())
}

fn cmd_userland_plan(log: &Logger, args: &[&str]) -> Result<()> {
    let mut opts = baseopts();

    opts.optopt("m", "", "memo file for build progress", "MEMOFILE");
    opts.optflag("F", "", "skip failures");

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] \
            userland-plan [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    let skip_failures = res.opt_present("F");

    let memo: Option<Memo> = if let Some(mf) = res.opt_str("m") {
        memo_load(log, &mf)?
    } else {
        None
    };

    if res.free.is_empty() {
        bail!("provide package names");
    }

    let top = top()?;
    println!("helios repository root is: {}", top.display());

    /*
     * Load the FMRI-to-path mapping file:
     */
    let mdf = top_path(&["projects", "userland", "components",
        "mapping.json"])?;
    ensure::removed(log, &mdf.to_str().unwrap())?;
    let compdir = top_path(&["projects", "userland", "components"])?;
    userland_gmake(log, &compdir, "mapping.json")?;
    let f = File::open(&mdf)?;
    let um: Vec<UserlandMapping> = serde_json::from_reader(&f)?;

    let mut memo = if let Some(memo) = memo {
        memo
    } else {
        let mut q: VecDeque<MemoQueueEntry> = VecDeque::new();

        for pkg in res.free.iter() {
            q.push_back(MemoQueueEntry {
                fmri: pkg.clone(),
                optional: false
            });
        }

        Memo {
            q,
            fails: VecDeque::new(),
            seen: BTreeSet::new(),
        }
    };

    loop {
        if let Some(mf) = res.opt_str("m") {
            memo_store(log, &mf, &memo)?;
        }

        let mqe = if let Some(mqe) = memo.q.pop_front() {
            mqe
        } else {
            break;
        };

        /*
         * Remove the pkg:/ prefix if present.
         */
        let pkg = mqe.fmri.trim_start_matches("pkg:/");

        if memo.seen.contains(pkg) {
            continue;
        }
        memo.seen.insert(pkg.to_string());

        info!(log, "planning: {} (optional? {:?}", pkg, mqe.optional);

        let mats: Vec<_> = um.iter()
            .filter(|m| &m.fmri == pkg)
            .filter(|m| m.repo.as_deref()
                .map(|repo| !repo.contains("encumbered"))
                .unwrap_or(true))
            .collect();

        if mats.is_empty() {
            if mqe.optional {
                warn!(log, "no match for optional FMRI {} (skipping)", pkg);
                continue;
            }

            bail!("no match for FMRI {}", pkg);
        } else if mats.len() > 1 {
            bail!("{} matches for FMRI {}: {:?}", mats.len(), pkg, mats);
        }

        info!(log, "match: {:?}", mats[0]);

        /*
         * Check for this package in the build repository...
         * XXX Do not build gate packages this way for now...
         */
        if mats[0].name != "illumos-gate" &&
            !repo_contains(log, &format!("pkg:/{}", pkg))?
        {
            let p = &mats[0].path;

            if let Err(e) = build(log, p) {
                if skip_failures {
                    error!(log, "building {} in {} failed", pkg, p);
                    memo.fails.push_back(mqe);
                    continue;
                }

                bail!("building {} in {} failed", pkg, p);
            }
        }

        /*
         * Get the dependencies for this package and put them in the queue...
         */
        let contents = repo_contents(log, &format!("pkg:/{}", pkg))?;

        for a in contents.iter() {
            match &a {
                Action::Depend(ad) => {
                    if ad.type_ == "incorporate" {
                        /*
                         * Incorporated dependencies constrain versions, but do
                         * not themselves require installation.
                         */
                        continue;
                    }

                    if ad.type_ != "require" &&
                        ad.type_ != "require-any" &&
                        ad.type_ != "group" &&
                        ad.type_ != "group-any" &&
                        ad.type_ != "optional" &&
                        ad.type_ != "conditional"
                    {
                        bail!("unexpected depend type: {:?}", ad);
                    }

                    for dep in ad.fmris().iter() {
                        let dep = dep.trim_start_matches("pkg:/");
                        let dep = if let Some(idx) = dep.find('@') {
                            &dep[0..idx]
                        } else {
                            dep
                        };

                        if memo.seen.contains(dep) {
                            continue;
                        }

                        info!(log, "adding ({}): {} -> {}", ad.type_, pkg, dep);
                        let depopt = ad.type_ == "optional";
                        memo.q.push_back(MemoQueueEntry {
                            fmri: dep.to_string(),
                            optional: depopt
                        });
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn cmd_setup(log: &Logger, args: &[&str]) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] setup [OPTIONS]"));
    };

    let res = opts.parse(args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    let top = top()?;
    println!("helios repository root is: {}", top.display());

    /*
     * Read the projects file which contains the URLs of the repositories we
     * need to clone.
     */
    let p: Projects = read_toml(top_path(&["config", "projects.toml"])?)?;
    println!("{:#?}", p);

    ensure_dir(&["projects"])?;
    ensure_dir(&["tmp"])?;

    for (name, project) in p.project.iter() {
        let path = top_path(&["projects", &name])?;
        let url = project.url(false)?;
        let tmp = ensure_dir(&["tmp", &name])?;

        if exists_dir(&path)? {
            println!("clone {} exists already at {}", url, path.display());
        } else {
            println!("cloning {} at {}...", url, path.display());

            let mut child = Command::new("git")
                .arg("clone")
                .arg(&url)
                .arg(&path)
                .spawn()?;

            let exit = child.wait()?;
            if !exit.success() {
                bail!("clone of {} to {} failed", url, path.display());
            }

            println!("clone ok!");
        }

        if project.site_sh {
            let mut ssp = path.clone();
            ssp.push("lib");
            ssp.push("site.sh");
            info!(log, "creating config file at {}", ssp.display());

            let mut site_sh = String::new();
            site_sh += "PFEXEC=/usr/bin/pfexec\n";
            site_sh += "PKGPUBLISHER=helios-dev\n";
            site_sh += "PUBLISHER_EMAIL=jmc@oxide.computer\n";
            site_sh += &format!("RELVER={}\n", RELVER);
            site_sh += &format!("DASHREV={}\n", DASHREV);
            site_sh += "PVER=$RELVER.$DASHREV\n";
            site_sh += "IPS_REPO=https://pkg.oxide.computer/helios-dev-2\n";
            site_sh += &format!("TMPDIR={}\n", &tmp.to_str().unwrap());
            site_sh += "DTMPDIR=$TMPDIR\n";

            ensure::file_str(log, &site_sh, &ssp, 0o644,
                ensure::Create::Always)?;
        }
    }

    /*
     * Create the package repository that will contain the final output
     * packages after build and transformations are applied.
     */
    let publisher = "helios-dev";
    ensure_dir(&["packages"])?;
    for repo in &["os", "other", "combined"] {
        let repo_path = top_path(&["packages", repo])?;
        create_ips_repo(log, &repo_path, &publisher, false)?;
    }

    /*
     * Create the pkgmogrify template that we need to replace the pkg(5)
     * publisher name when promoting packages from a build repository to the
     * central repository.
     */
    let mog = format!("<transform set name=pkg.fmri -> \
        edit value pkg://[^/]+/ pkg://{}/>\n", publisher);
    let mogpath = top_path(&["packages", "publisher.mogrify"])?;
    ensure::file_str(log, &mog, &mogpath, 0o644, ensure::Create::Always)?;

    let mog = format!("<transform depend fmri=.*-151035.0$ -> \
        edit fmri 151035.0$ {}.{}>\n", RELVER, DASHREV);
    let mogpath = top_path(&["packages", "osver.mogrify"])?;
    ensure::file_str(log, &mog, &mogpath, 0o644, ensure::Create::Always)?;

    let mogpath = top_path(&["packages", "os-conflicts.mogrify"])?;
    ensure::symlink(log, &mogpath, "../tools/packages/os-conflicts.mogrify")?;

    /*
     * Perform setup in userland repository.
     */
    // let userland_path = top_path(&["projects", "userland"])?;
    // if exists_dir(&userland_path)? {
    //     let p = userland_path.to_str().unwrap(); /* XXX */

    //     ensure::run(log, &["/usr/bin/gmake", "-C", &p, "setup"])?;
    // }

    Ok(())
}

struct CommandInfo {
    name: String,
    desc: String,
    func: fn(&Logger, &[&str]) -> Result<()>,
    hide: bool,
}

fn main() -> Result<()> {
    let mut opts = baseopts();
    opts.parsing_style(getopts::ParsingStyle::StopAtFirstFree);

    let mut handlers: Vec<CommandInfo> = Vec::new();
    handlers.push(CommandInfo {
        name: "setup".into(),
        desc: "clone required repositories and run setup tasks".into(),
        func: cmd_setup,
        hide: false,
    });
    handlers.push(CommandInfo {
        name: "zone".into(),
        desc: "zone".into(),
        func: cmd_zone,
        hide: true,
    });
    handlers.push(CommandInfo {
        name: "bldenv".into(),
        desc: "enter a bldenv shell for illumos so you can run dmake".into(),
        func: cmd_illumos_bldenv,
        hide: false,
    });
    handlers.push(CommandInfo {
        name: "build-illumos".into(),
        desc: "run a full nightly(1) and produce packages".into(),
        func: cmd_build_illumos,
        hide: false,
    });
    handlers.push(CommandInfo {
        name: "merge-illumos".into(),
        desc: "merge DEBUG and non-DEBUG packages into one repository".into(),
        func: cmd_promote_illumos,
        hide: false,
    });
    handlers.push(CommandInfo {
        name: "build".into(),
        desc: "build".into(),
        func: cmd_build,
        hide: true,
    });
    handlers.push(CommandInfo {
        name: "build-omnios".into(),
        desc: "build-omnios".into(),
        func: cmd_build_omnios,
        hide: true,
    });
    handlers.push(CommandInfo {
        name: "download_metadata".into(),
        desc: "download_metadata".into(),
        func: cmd_download_metadata,
        hide: true,
    });
    handlers.push(CommandInfo {
        name: "archive".into(),
        desc: "archive".into(),
        func: cmd_archive,
        hide: true,
    });
    handlers.push(CommandInfo {
        name: "userland-plan".into(),
        desc: "userland-plan".into(),
        func: cmd_userland_plan,
        hide: true,
    });
    handlers.push(CommandInfo {
        name: "userland-promote".into(),
        desc: "userland-promote".into(),
        func: cmd_userland_promote,
        hide: true,
    });

    let usage = || {
        let mut out = String::new();
        out += "Usage: helios [OPTIONS] COMMAND [OPTIONS] [ARGS...]\n\n";
        for ci in handlers.iter() {
            if ci.hide {
                continue;
            }

            out += &format!("    {:<16} {}\n", ci.name, ci.desc);
        }
        println!("{}", opts.usage(&out));
    };

    let res = opts.parse(std::env::args().skip(1))?;
    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if res.free.is_empty() {
        usage();
        bail!("choose a command");
    }

    let args = res.free[1..].iter().map(|s| s.as_str()).collect::<Vec<_>>();

    let log = init_log();

    for ci in handlers {
        if ci.name != res.free[0] {
            continue;
        }

        return (ci.func)(&log, args.as_slice());
    }

    bail!("command \"{}\" not understood", res.free[0]);
}
