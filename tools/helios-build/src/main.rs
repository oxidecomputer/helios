/*
 * Copyright 2025 Oxide Computer Company
 */

mod common;
use common::*;

use anyhow::{bail, Context, Result};
use helios_build_utils::metadata::{self, ArchiveType};
use helios_build_utils::tree;
use serde::Deserialize;
use slog::Logger;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Instant, SystemTime};
use time::{format_description, OffsetDateTime};
use walkdir::WalkDir;

mod archive;
pub mod ensure;
mod expand;
pub mod illumos;
mod zfs;

use expand::Expansion;

const PKGREPO: &str = "/usr/bin/pkgrepo";
const PKGRECV: &str = "/usr/bin/pkgrecv";
const PKGDEPOTD: &str = "/usr/lib/pkg.depotd";

const DASHREV: u32 = 0;

#[derive(Copy, Clone)]
enum RelVer {
    V1,
    V2,
}

impl std::fmt::Display for RelVer {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::result::Result<(), std::fmt::Error> {
        write!(
            f,
            "{}",
            match self {
                RelVer::V1 => 1,
                RelVer::V2 => 2,
            }
        )
    }
}

const DATE_FORMAT_STR: &'static str = "[year]-[month]-[day]";
const TIME_FORMAT_STR: &'static str = "[hour]:[minute]:[second]";

fn baseopts() -> getopts::Options {
    let mut opts = getopts::Options::new();

    /*
     * We should always have a --help flag everywhere.
     */
    opts.optflag("", "help", "display usage information");

    opts
}

use std::ffi::OsStr;
use std::path::{Component, PathBuf};

const NO_PATH: Option<PathBuf> = None;

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

fn rel_path<P: AsRef<Path>>(
    p: Option<P>,
    components: &[&str],
) -> Result<PathBuf> {
    let mut top =
        if let Some(p) = p { p.as_ref().to_path_buf() } else { top()? };
    for c in components {
        top.push(c);
    }
    Ok(top)
}

fn top_path(components: &[&str]) -> Result<PathBuf> {
    let mut top = top()?;
    for c in components {
        top.push(c);
    }
    Ok(top)
}

fn abs_path<P: AsRef<Path>>(p: P) -> Result<PathBuf> {
    let p = p.as_ref();
    Ok(if p.is_absolute() {
        p.to_path_buf()
    } else {
        let mut pp = std::env::current_dir()?;
        pp.push(p);
        pp.canonicalize()?
    })
}

fn gate_name<P: AsRef<Path>>(p: P) -> Result<String> {
    let p = abs_path(p)?;
    p.canonicalize()?;
    if !p.is_dir() {
        bail!("{:?} is not a directory?", p);
    }
    if let Some(basename) = p.file_name() {
        if let Some(basename) = basename.to_str() {
            return Ok(basename.trim().to_string());
        }
    }
    bail!("could not get base name of {:?}", p);
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
     * Attempt to update this repository from upstream when running setup?
     */
    #[serde(default)]
    auto_update: bool,

    /*
     * When cloning or updating this repository, pin to this revision. The
     * revision can be a commit hash, a refname (such as a branch), or any
     * other valid revision as described in gitrevisions(7).
     */
    #[serde(default)]
    rev: Option<String>,

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

    /*
     * Run "cargo build" in this project to produce tools that we need:
     */
    #[serde(default)]
    cargo_build: bool,
    #[serde(default)]
    use_debug: bool,

    /*
     * If this environment variable is set to "no", we will skip cloning and
     * building this project.
     */
    unless_env: Option<String>,

    #[serde(default)]
    fixup: Vec<Fixup>,
}

#[derive(Debug, Deserialize)]
struct Fixup {
    from_commit: String,
    to_branch: String,
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

    fn skip(&self) -> bool {
        self.skip_reason().is_some()
    }

    fn skip_reason(&self) -> Option<String> {
        if let Some(key) = self.unless_env.as_deref() {
            if let Ok(value) = std::env::var(key) {
                let value = value.to_ascii_lowercase();
                if value == "no" || value == "0" || value == "false" {
                    return Some(format!("{key:?} is set to {value:?}"));
                }
            }
        }

        None
    }
}

fn ensure_dir(components: &[&str]) -> Result<PathBuf> {
    let dir = top_path(components)?;
    if !exists_dir(&dir)? {
        std::fs::create_dir(&dir)?;
    }
    Ok(dir)
}

fn create_ips_repo<P, S>(
    log: &Logger,
    path: P,
    publ: S,
    torch: bool,
) -> Result<()>
where
    P: AsRef<Path>,
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

/*
 * illumos builds can be run to produce DEBUG or non-DEBUG ("release") bits.
 * This command will take a multi-proto build (i.e., one which has produced both
 * DEBUG and non-DEBUG bits) and merge them into a single unified set of
 * packages.  That way, the choice of DEBUG or non-DEBUG can be made with "pkg
 * change-variant" during ramdisk construction or on a mutable root system.
 */
fn cmd_merge_illumos(ca: &CommandArg) -> Result<()> {
    let mut opts = baseopts();
    opts.optopt("g", "", "use an external gate directory", "DIR");
    opts.optopt("s", "", "tempdir name suffix", "SUFFIX");
    opts.optopt("o", "", "output repository", "REPO");
    opts.optopt("p", "", "output publisher name", "PUBLISHER");

    let usage = || {
        println!(
            "{}",
            opts.usage("Usage: helios [OPTIONS] merge-illumos [OPTIONS]")
        );
    };

    let log = ca.log;
    let res = opts.parse(ca.args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    /*
     * The illumos build creates packages with the "on-nightly" publisher, which
     * we then replace with the desired publisher name as part of merging the
     * DEBUG- and non-DEBUG bits into a unified set of packages.
     */
    let input_publisher = "on-nightly";

    let gate = if let Some(gate) = res.opt_str("g") {
        abs_path(gate)?
    } else {
        top_path(&["projects", "illumos"])?
    };

    /*
     * We want a temporary directory name that does not overlap with other
     * concurrent usage of this tool.
     */
    let tillumos = if let Some(suffix) = res.opt_str("s") {
        /*
         * If the user provides a specific suffix, just use it as-is.
         */
        format!("illumos.{}", suffix)
    } else if let Some(gate) = res.opt_str("g") {
        /*
         * If an external gate is selected, we assume that the base directory
         * name is unique; e.g., "/ws/ftdi", or "/ws/upstream" would yield
         * "ftdi" or "upstream".  This allows repeat runs for the same external
         * workspace to reuse the previous temporary directory.
         */
        format!("illumos.gate-{}", gate_name(gate)?)
    } else {
        "illumos".to_string()
    };

    ensure_dir(&["tmp", &tillumos])?;
    let repo_merge = top_path(&["tmp", &tillumos, "nightly-merged"])?;

    let repo_d =
        rel_path(Some(&gate), &["packages", "i386", "nightly", "repo.redist"])?;
    let repo_nd = rel_path(
        Some(&gate),
        &["packages", "i386", "nightly-nd", "repo.redist"],
    )?;

    /*
     * Merge the packages from the DEBUG and non-DEBUG builds into a single
     * staging repository using the IPS variant feature.
     */
    info!(log, "recreating merging repository at {:?}", &repo_merge);
    create_ips_repo(log, &repo_merge, &input_publisher, true)?;

    ensure::run(
        log,
        &[
            "/usr/bin/pkgmerge",
            "-d",
            &repo_merge.to_str().unwrap(),
            "-s",
            &format!("debug.illumos=false,{}/", repo_nd.to_str().unwrap()),
            "-s",
            &format!("debug.illumos=true,{}/", repo_d.to_str().unwrap()),
        ],
    )?;

    info!(log, "transforming packages for publishing...");

    let mog_publisher = if let Some(publisher) = res.opt_str("p") {
        info!(log, "using custom publisher {:?}", publisher);
        let file = top_path(&["tmp", &tillumos, "custom-publisher.mogrify"])?;
        regen_publisher_mog(log, Some(&file), &publisher)?;
        file
    } else {
        info!(log, "using default publisher");
        top_path(&["packages", "publisher.mogrify"])?
    };

    let mog_conflicts = top_path(&["packages", "os-conflicts.mogrify"])?;
    let mog_deps = top_path(&["packages", "os-deps.mogrify"])?;
    let repo = if let Some(repo) = res.opt_str("o") {
        PathBuf::from(repo)
    } else {
        top_path(&["packages", "os"])?
    };

    ensure::run(
        log,
        &[
            PKGRECV,
            "-s",
            &repo_merge.to_str().unwrap(),
            "-d",
            &repo.to_str().unwrap(),
            "--mog-file",
            &mog_publisher.to_str().unwrap(),
            "--mog-file",
            &mog_conflicts.to_str().unwrap(),
            "--mog-file",
            &mog_deps.to_str().unwrap(),
            "-m",
            "latest",
            "*",
        ],
    )?;
    ensure::run(log, &[PKGREPO, "refresh", "-s", &repo.to_str().unwrap()])?;

    /*
     * Clean up the temporary merged repo files:
     */
    std::fs::remove_dir_all(&repo_merge).ok();
    if res.opt_present("p") {
        maybe_unlink(&mog_publisher)?;
    }

    Ok(())
}

fn ncpus() -> Result<u32> {
    /*
     * XXX Replace with kstat check.
     */
    let out =
        Command::new("/usr/sbin/psrinfo").env_clear().arg("-t").output()?;

    if !out.status.success() {
        bail!("could not count CPUs: {}", out.info());
    }

    let stdout = String::from_utf8(out.stdout)?;
    Ok(stdout.trim().parse().context("psrinfo parse failure")?)
}

#[derive(Clone, Copy)]
enum BuildType {
    Quick,
    QuickDebug,
    Full,
    Release,
}

impl BuildType {
    fn script_name(&self) -> &str {
        use BuildType::*;

        /*
         * The build environment will be different, depending on the build type.
         * Use a different file for each type to make that clear.
         */
        match self {
            Quick => "illumos-quick.sh",
            QuickDebug => "illumos-quick-debug.sh",
            Full => "illumos.sh",
            Release => "illumos-release.sh",
        }
    }
}

fn regen_publisher_mog<P: AsRef<Path>>(
    log: &Logger,
    mogfile: Option<P>,
    publisher: &str,
) -> Result<()> {
    /*
     * Create the pkgmogrify template that we need to replace the pkg(5)
     * publisher name when promoting packages from a build repository to the
     * central repository.
     */
    let mog = format!(
        "<transform set name=pkg.fmri -> \
        edit value pkg://[^/]+/ pkg://{}/>\n",
        publisher
    );
    let mogpath = if let Some(mogfile) = mogfile {
        mogfile.as_ref().to_path_buf()
    } else {
        top_path(&["packages", "publisher.mogrify"])?
    };
    ensure::file_str(log, &mog, &mogpath, 0o644, ensure::Create::Always)?;
    Ok(())
}

fn determine_release_version() -> Result<RelVer> {
    let relpath = "/etc/os-release";
    let relfile = std::fs::read_to_string(relpath)?;
    let map: HashMap<_, _> =
        relfile.lines().filter_map(|l| l.split_once('=')).collect();

    let Some(id) = map.get("ID") else {
        bail!("ID missing from {relpath:?}");
    };
    if *id != "helios" {
        bail!("expect to build on \"helios\", not {id:?} (from {relpath:?})");
    }

    let Some(version_id) = map.get("VERSION_ID") else {
        bail!("VERSION_ID missing from {relpath:?}");
    };
    Ok(match *version_id {
        "1" => RelVer::V1,
        "2" => RelVer::V2,
        other => bail!("unexpected VERSION_ID {other:?} in {relpath:?}"),
    })
}

fn regen_illumos_sh<P: AsRef<Path>>(
    log: &Logger,
    gate: P,
    bt: BuildType,
    relver: RelVer,
    parent_branch: &Option<String>,
) -> Result<PathBuf> {
    let gate = gate.as_ref();
    let path_env = rel_path(Some(gate), &[bt.script_name()])?;

    let maxjobs = ncpus()?;

    let (pkgvers, vers, banner) = match bt {
        /*
         * Though git does not support an SVN- or Mercurial-like revision
         * number, our history is sufficiently linear that we can approximate
         * one anyway.  Use that to set an additional version number component
         * beyond the release version, and as the value for "uname -v":
         */
        BuildType::Release => {
            let pkgvers = if let Some(br) = parent_branch.as_deref() {
                /*
                 * If this is a respin with backports, we need a more complex
                 * version number.  First, determine where we branched from the
                 * parent, and determine the commit count at that point:
                 */
                let bp = git_branch_point(&gate, br, "HEAD")?;
                let rnum = git_commit_count(&gate, &bp)?;
                info!(log, "base commit: {bp:?}, branch {br:?}, count {rnum}");

                /*
                 * Next, calculate a fourth octet based on the number of commits
                 * from our common branch point up to the current commit on the
                 * respin branch:
                 */
                let extra = git_commit_count(&gate, &format!("{bp}..HEAD"))?;

                format!("{relver}.{DASHREV}.{rnum}.{extra}")
            } else {
                /*
                 * For regular release builds, just count the number of commits
                 * in the current branch:
                 */
                let rnum = git_commit_count(&gate, "HEAD")?;

                format!("{relver}.{DASHREV}.{rnum}")
            };
            let vers = format!("helios-{pkgvers}");
            (pkgvers, vers, "Oxide Helios Version ^v ^w-bit")
        }
        /*
         * If this is a quick build that one intends to install on the local
         * system and iterate on, set the revision number to an extremely high
         * number that is obviously not related to the production package commit
         * numbers:
         */
        BuildType::Quick | BuildType::QuickDebug | BuildType::Full => {
            let pkgvers = format!("{relver}.{DASHREV}.999999");
            let vers = "$(git describe --long --all HEAD | cut -d/ -f2-)";
            (pkgvers, vers.into(), "Oxide Helios Version ^v ^w-bit (onu)")
        }
    };

    /*
     * Construct an environment file to build illumos-gate.
     */
    let mut env = String::new();
    match bt {
        BuildType::Full => env += "export NIGHTLY_OPTIONS='-nCDAprt'\n",
        BuildType::Release => env += "export NIGHTLY_OPTIONS='-nCDAprt'\n",
        BuildType::Quick => env += "export NIGHTLY_OPTIONS='-nCAprt'\n",
        BuildType::QuickDebug => env += "export NIGHTLY_OPTIONS='-nCADFprt'\n",
    }
    env += &format!("export CODEMGR_WS='{}'\n", gate.to_str().unwrap());
    env += "export MACH=\"$(uname -p)\"\n";
    env += "export GNUC_ROOT=/opt/gcc-10\n";
    env += "export PRIMARY_CC=gcc10,$GNUC_ROOT/bin/gcc,gnu\n";
    env += "export PRIMARY_CCC=gcc10,$GNUC_ROOT/bin/g++,gnu\n";
    env += "export SHADOW_CCS=\n";
    env += "export SHADOW_CCCS=\n";
    match bt {
        BuildType::Quick | BuildType::QuickDebug => {
            /*
             * Skip the shadow compiler and smatch for quick builds:
             */
        }
        BuildType::Full | BuildType::Release => {
            /*
             * Enable the shadow compiler(s) for full builds:
             */
            const GCC_VERSIONS: &[u32] = &[14];

            for v in GCC_VERSIONS {
                env += &format!(
                    "SHADOW_CCS+=\" gcc{v},/opt/gcc-{v}/bin/gcc,gnu\"\n"
                );
                env += &format!(
                    "SHADOW_CCCS+=\" gcc{v},/opt/gcc-{v}/bin/g++,gnu\"\n"
                );
            }

            /*
             * Enable smatch checks for full builds:
             */
            env += "SMATCHBIN=$CODEMGR_WS/usr/src/tools/proto/\
                root_$MACH-nd/opt/onbld/bin/$MACH/smatch\n";
            env += "SHADOW_CCS+=\" smatch,$SMATCHBIN,smatch\"\n";
        }
    }
    env += "export BUILDVERSION_EXEC=\"git describe --all --long --dirty\"\n";
    env += &format!("export DMAKE_MAX_JOBS={}\n", maxjobs);
    env += "export ENABLE_SMB_PRINTING='#'\n";
    match relver {
        RelVer::V1 => {
            env += "export PERL_VERSION=5.32\n";
        }
        RelVer::V2 => {
            env += "export PERL_VERSION=5.36\n";
        }
    }
    env += "export PERL_PKGVERS=\n";
    env += "export PERL_VARIANT=-thread-multi\n";
    env += "export BUILDPERL32='#'\n";
    env += "export JAVA_ROOT=/usr/jdk/openjdk11.0\n";
    env += "export JAVA_HOME=$JAVA_ROOT\n";
    env += "export BLD_JAVA_11=\n";
    env += "export BUILDPY2='#'\n";
    env += "export BUILDPY3=\n";
    env += "export BUILDPY2TOOLS='#'\n";
    env += "export BUILDPY3TOOLS=\n";
    match relver {
        RelVer::V1 => {
            env += "export PYTHON3_VERSION=3.9\n";
            env += "export PYTHON3_PKGVERS=-39\n";
        }
        RelVer::V2 => {
            env += "export PYTHON3_VERSION=3.11\n";
            env += "export PYTHON3_PKGVERS=-311\n";
        }
    }
    env += "export PYTHON3_SUFFIX=\n";
    env += "export TOOLS_PYTHON=/usr/bin/python$PYTHON3_VERSION\n";
    env += "export STAFFER=\"$LOGNAME\"\n";
    env += "export MAILTO=\"${MAILTO:-$STAFFER}\"\n";
    env += "export BUILD_PROJECT=''\n";
    env += "export ATLOG=\"$CODEMGR_WS/log\"\n";
    env += "export LOGFILE=\"$ATLOG/nightly.log\"\n";
    env += "export BUILD_TOOLS='/opt'\n";
    env += "export MAKEFLAGS='ke'\n";
    env += "export PARENT_WS=''\n";
    env += "export REF_PROTO_LIST=\"$PARENT_WS/usr/src/proto_list_${MACH}\"\n";
    env += "export PARENT_ROOT=\"$PARENT_WS/proto/root_$MACH\"\n";
    env += "export PARENT_TOOLS_ROOT=\
        \"$PARENT_WS/usr/src/tools/proto/root_$MACH-nd\"\n";
    env += "export PKGARCHIVE=\"${CODEMGR_WS}/packages/${MACH}/nightly\"\n";
    env += &format!("export VERSION=\"{}\"\n", vers);
    env += &format!("export BOOTBANNER1=\"{}\"\n", banner);
    env += "export ROOT=\"$CODEMGR_WS/proto/root_${MACH}\"\n";
    env += "export SRC=\"$CODEMGR_WS/usr/src\"\n";
    env += "export MULTI_PROTO=\"yes\"\n";
    env += "export ONBLD_BIN=/opt/onbld/bin\n";
    env += "export ON_CLOSED_BINS=/opt/onbld/closed\n";
    env += &format!("export PKGVERS_BRANCH='{pkgvers}'\n");

    ensure::file_str(log, &env, &path_env, 0o644, ensure::Create::Always)?;

    Ok(path_env)
}

fn cmd_build_illumos(ca: &CommandArg) -> Result<()> {
    if std::env::var_os("CODEMGR_WS").is_some() {
        bail!("illumos build should not run from within the bldenv shell");
    }

    let mut opts = baseopts();
    opts.optflag("q", "quick", "quick build (no shadows, no DEBUG)");
    opts.optflag("d", "debug", "build a debug build (use with -q)");
    opts.optflag("r", "release", "build a release build");
    opts.optopt("g", "", "use an external gate directory", "DIR");
    opts.optflag("i", "incremental", "perform an incremental build");
    opts.optopt("b", "", "use a parent branch for respin versioning", "BRANCH");

    let usage = || {
        println!(
            "{}",
            opts.usage("Usage: helios [OPTIONS] build-illumos [OPTIONS]")
        );
    };

    let log = ca.log;
    let res = opts.parse(ca.args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    if res.opt_present("q") && res.opt_present("r") {
        bail!("you cannot request a release build (-r) and a quick build (-q)");
    }

    if res.opt_present("d") && res.opt_present("r") {
        bail!("you cannot request a release build (-r) and a debug build (-d)");
    }

    if res.opt_present("d") && !res.opt_present("q") {
        bail!("requesting a debug build (-d) requires -q");
    }

    let bt = if res.opt_present("q") {
        if res.opt_present("d") {
            BuildType::QuickDebug
        } else {
            BuildType::Quick
        }
    } else if res.opt_present("r") {
        BuildType::Release
    } else {
        BuildType::Full
    };

    let relver = determine_release_version()?;

    let gate = if let Some(gate) = res.opt_str("g") {
        abs_path(gate)?
    } else {
        top_path(&["projects", "illumos"])?
    };
    let parent = res.opt_str("b");
    let env_sh = regen_illumos_sh(log, &gate, bt, relver, &parent)?;

    let script = format!(
        "cd {} && ./usr/src/tools/scripts/nightly{} {}",
        gate.to_str().unwrap(),
        if res.opt_present("i") { " -i" } else { "" },
        env_sh.to_str().unwrap()
    );

    ensure::run(log, &["/sbin/sh", "-c", &script])?;

    Ok(())
}

fn create_transformed_repo(
    log: &Logger,
    gate: &Path,
    tmpdir: &Path,
    debug: bool,
    refresh: bool,
) -> Result<PathBuf> {
    let repo = rel_path(Some(tmpdir), &["repo.redist"])?;
    create_ips_repo(log, &repo, "on-nightly", true)?;

    /*
     * These pkgmogrify(1) scripts will drop any conflicting actions:
     */
    let mog_conflicts = top_path(&["packages", "os-conflicts.mogrify"])?;
    let mog_deps = top_path(&["packages", "os-deps.mogrify"])?;

    info!(log, "transforming packages for installation...");
    let which = if debug { "nightly" } else { "nightly-nd" };
    let repo_nd =
        rel_path(Some(gate), &["packages", "i386", which, "repo.redist"])?;
    ensure::run(
        log,
        &[
            PKGRECV,
            "-s",
            &repo_nd.to_str().unwrap(),
            "-d",
            &repo.to_str().unwrap(),
            "--mog-file",
            &mog_conflicts.to_str().unwrap(),
            "--mog-file",
            &mog_deps.to_str().unwrap(),
            "-m",
            "latest",
            "*",
        ],
    )?;
    if refresh {
        ensure::run(log, &[PKGREPO, "refresh", "-s", &repo.to_str().unwrap()])?;
    }

    Ok(repo)
}

fn cmd_illumos_onu(ca: &CommandArg) -> Result<()> {
    let mut opts = baseopts();
    opts.optopt("t", "", "boot environment name", "NAME");
    opts.optflag("P", "", "prepare packages only");
    opts.optflag("D", "", "prepare packages and run a depot");
    opts.optflag("d", "", "use DEBUG packages");
    opts.optopt("g", "", "use an external gate directory", "DIR");
    opts.optopt("l", "", "depot listen port (default 7891)", "PORT");
    opts.optopt("s", "", "tempdir name suffix", "SUFFIX");

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] onu [OPTIONS]"));
    };

    let log = ca.log;
    let res = opts.parse(ca.args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    let gate = if let Some(gate) = res.opt_str("g") {
        abs_path(gate)?
    } else {
        top_path(&["projects", "illumos"])?
    };

    /*
     * We want a temporary directory name that does not overlap with other
     * concurrent usage of this tool.
     */
    let tonu = if let Some(suffix) = res.opt_str("s") {
        /*
         * If the user provides a specific suffix, just use it as-is.
         */
        format!("onu.{}", suffix)
    } else if let Some(gate) = res.opt_str("g") {
        /*
         * If an external gate is selected, we assume that the base directory
         * name is unique; e.g., "/ws/ftdi", or "/ws/upstream" would yield
         * "ftdi" or "upstream".  This allows repeat runs for the same external
         * workspace to reuse the previous temporary directory.
         */
        format!("onu.gate-{}", gate_name(gate)?)
    } else if let Some(port) = res.opt_str("l") {
        /*
         * If the internal gate is in use, but a non-default port number is
         * specified, use that port for the temporary suffix.
         */
        format!("onu.port-{}", port)
    } else {
        "onu".to_string()
    };

    let count = ["t", "P", "D"].iter().filter(|o| res.opt_present(o)).count();
    if count == 0 {
        usage();
        bail!("must specify one of -t, -P, or -D");
    } else if count > 1 {
        usage();
        bail!("-t, -P, and -D, are mutually exclusive");
    }

    /*
     * In order to install development illumos bits, we first need to elide any
     * files that would conflict with packages delivered from other
     * consolidations.  To do this, we create an onu-specific repository:
     */
    info!(log, "creating temporary repository...");
    let repo = create_transformed_repo(
        log,
        &gate,
        &ensure_dir(&["tmp", &tonu])?,
        res.opt_present("d"),
        true,
    )?;

    if res.opt_present("P") {
        info!(log, "transformed packages available for onu at: {:?}", &repo);
        return Ok(());
    }

    if res.opt_present("D") {
        let port = if let Some(port) = res.opt_str("l") {
            let port: u16 = port.parse()?;
            if port == 0 {
                bail!("port number (-l) must be a positive integer");
            }
            port
        } else {
            7891
        };

        /*
         * Perform a construction similar to the one we do for repository
         * temporary files, but for depot logs.
         */
        let tdepot = if let Some(suffix) = res.opt_str("s") {
            format!("depot.{}", suffix)
        } else if let Some(gate) = res.opt_str("g") {
            format!("depot.gate-{}", gate_name(gate)?)
        } else if let Some(port) = res.opt_str("l") {
            format!("depot.port-{}", port)
        } else {
            "depot".to_string()
        };

        info!(log, "starting pkg.depotd on packages at: {:?}", &repo);

        /*
         * Run a pkg.depotd to serve the packages we have just transformed.
         */
        ensure_dir(&["tmp", &tdepot])?;
        let logdir = ensure_dir(&["tmp", &tdepot, "log"])?;
        let mut access = logdir.clone();
        access.push("access");
        let rootdir = ensure_dir(&["tmp", &tdepot, "root"])?;

        info!(log, "access log file is {:?}", &access);
        info!(log, "listening on port {}", port);
        info!(log, "^C to quit");

        return Err(Command::new(PKGDEPOTD)
            /*
             * Setting this environment variable prevents the depot from
             * daemonising.
             */
            .env("PKGDEPOT_CONTROLLER", "1")
            .arg("-d")
            .arg(&repo)
            .arg("-p")
            .arg(port.to_string())
            .arg("--log-access")
            .arg(access)
            .arg("--log-error")
            .arg("stderr")
            .arg("--readonly")
            .arg("true")
            .arg("--writable-root")
            .arg(&rootdir)
            .exec()
            .into());
    }

    let bename = if let Some(bename) = res.opt_str("t") {
        bename
    } else {
        usage();
        bail!("must specify a boot environment name (-t)");
    };

    /*
     * onu(1) will create a new boot environment, adjusting it to accept nightly
     * packages, and then install the packages.  It must be run with root
     * privileges as it modifies the system.
     */
    info!(log, "installing packages...");
    let onu = rel_path(
        Some(&gate),
        &[
            "usr",
            "src",
            "tools",
            "proto",
            "root_i386-nd",
            "opt",
            "onbld",
            "bin",
            "onu",
        ],
    )?;

    let onu_dir = top_path(&["tmp", &tonu])?;
    ensure::run(
        log,
        &[
            "pfexec",
            &onu.to_str().unwrap(),
            "-v",
            "-d",
            &onu_dir.to_str().unwrap(),
            "-t",
            &bename,
        ],
    )?;

    info!(log, "onu complete!  you must now reboot");
    Ok(())
}

fn cmd_illumos_genenv(ca: &CommandArg) -> Result<()> {
    let mut opts = baseopts();
    opts.optopt("g", "", "use an external gate directory", "DIR");
    opts.optopt("b", "", "use a parent branch for respin versioning", "BRANCH");

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] genenv [OPTIONS]"));
    };

    let res = opts.parse(ca.args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    let relver = determine_release_version()?;

    let gate = if let Some(gate) = res.opt_str("g") {
        abs_path(gate)?
    } else {
        top_path(&["projects", "illumos"])?
    };

    let parent = res.opt_str("b");
    regen_illumos_sh(ca.log, &gate, BuildType::Quick, relver, &parent)?;
    regen_illumos_sh(ca.log, &gate, BuildType::QuickDebug, relver, &parent)?;
    regen_illumos_sh(ca.log, &gate, BuildType::Full, relver, &parent)?;
    regen_illumos_sh(ca.log, &gate, BuildType::Release, relver, &parent)?;

    info!(ca.log, "ok");
    Ok(())
}

fn cmd_illumos_bldenv(ca: &CommandArg) -> Result<()> {
    if std::env::var_os("CODEMGR_WS").is_some() {
        bail!("bldenv should not run from within the bldenv shell");
    }

    let mut opts = baseopts();
    opts.optflag("q", "quick", "quick build (no shadows, no DEBUG)");
    opts.optflag("d", "debug", "build a debug build");
    opts.optflag("r", "release", "build a release build");
    opts.optopt("b", "", "use a parent branch for respin versioning", "BRANCH");

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] bldenv [OPTIONS]"));
    };

    let res = opts.parse(ca.args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    if res.opt_present("q") && res.opt_present("r") {
        bail!("you cannot request a release build (-r) and a quick build (-q)");
    }

    if res.opt_present("d") && res.opt_present("r") {
        bail!("you cannot request a release build (-r) and a debug build (-d)");
    }

    let t = if res.opt_present("q") {
        if res.opt_present("d") {
            BuildType::QuickDebug
        } else {
            BuildType::Quick
        }
    } else if res.opt_present("r") {
        BuildType::Release
    } else {
        BuildType::Full
    };

    let relver = determine_release_version()?;

    let gate = top_path(&["projects", "illumos"])?;
    let parent = res.opt_str("b");
    regen_illumos_sh(ca.log, &gate, t, relver, &parent)?;

    let env = rel_path(Some(&gate), &[t.script_name()])?;
    let src = rel_path(Some(&gate), &["usr", "src"])?;
    let bldenv =
        rel_path(Some(&gate), &["usr", "src", "tools", "scripts", "bldenv"])?;

    /*
     * bldenv(1) starts an interactive build shell with the correct environment
     * for running dmake(1) and other illumos build tools.  As such, we want to
     * exec(2) and replace this process rather than run it as a logged child
     * process.
     */
    let mut cmd = Command::new(&bldenv);
    if res.opt_present("d") && !res.opt_present("q") {
        cmd.arg("-d");
    }
    cmd.arg(env).current_dir(&src);
    let err = cmd.exec();
    bail!("exec failure: {:?}", err);
}

fn read_string(path: &Path) -> Result<String> {
    let f = File::open(path)?;
    let mut buf = String::new();
    let mut br = BufReader::new(&f);
    br.read_to_string(&mut buf)?;
    Ok(buf)
}

fn cargo_target_cmd(
    project: &str,
    command: &str,
    debug: bool,
) -> Result<String> {
    let bin = top_path(&[
        "projects",
        project,
        "target",
        if debug { "debug" } else { "release" },
        command,
    ])?;
    if !bin.is_file() {
        bail!("binary {:?} does not exist.  run \"gmake setup\"?", bin);
    }
    Ok(bin.to_str().unwrap().to_string())
}

/*
 * If we have been provided an extra proto directory, we want to include all of
 * the files and directories and symbolic links that have been assembled in that
 * proto area in the final image.  The image-builder tool cannot do this
 * natively because there is no way to know what metadata to use for the files
 * without some kind of explicit manifest provided as input to ensure_*
 * directives.
 *
 * For our purposes here, it seems sufficient to use the mode bits as-is and
 * just request that root own the files in the resultant image.  We generate a
 * partial template by walking the proto area, for inclusion when the "genproto"
 * feature is also enabled in our main template.
 */
fn genproto(proto: &Path, output_template: &Path) -> Result<()> {
    let rootdir = PathBuf::from("/");
    let mut steps: Vec<serde_json::Value> = Default::default();

    for ent in WalkDir::new(proto).min_depth(1).into_iter() {
        let ent = ent?;

        let relpath = tree::unprefix(proto, ent.path())?;
        if relpath == PathBuf::from("bin") {
            /*
             * On illumos, /bin is always a symbolic link to /usr/bin.
             */
            bail!(
                "proto {:?} contains a /bin directory; should use /usr/bin",
                proto
            );
        }

        /*
         * Use the relative path within the proto area as the absolute path
         * in the image; e.g., "proto/bin/id" would become "/bin/id" in the
         * image.
         */
        let path = tree::reprefix(proto, ent.path(), &rootdir)?;
        let path = path.to_str().unwrap();

        let md = ent.metadata()?;
        let mode = format!("{:o}", md.permissions().mode() & 0o777);
        if md.file_type().is_symlink() {
            let target = std::fs::read_link(ent.path())?;

            steps.push(serde_json::json!({
                "t": "ensure_symlink", "link": path, "target": target,
                "owner": "root", "group": "root",
            }));
        } else if md.file_type().is_dir() {
            /*
             * Some system directories are owned by groups other than "root".
             * The rules are not exact; this is an approximation to reduce
             * churn:
             */
            let group = if relpath.starts_with("var")
                || relpath.starts_with("etc")
                || relpath.starts_with("lib/svc/manifest")
                || relpath.starts_with("platform")
                || relpath.starts_with("kernel")
                || relpath == PathBuf::from("usr")
                || relpath == PathBuf::from("usr/share")
                || relpath.starts_with("usr/platform")
                || relpath.starts_with("usr/kernel")
                || relpath == PathBuf::from("opt")
            {
                "sys"
            } else if relpath.starts_with("lib") || relpath.starts_with("usr") {
                "bin"
            } else {
                "root"
            };

            steps.push(serde_json::json!({
                "t": "ensure_dir", "dir": path,
                "owner": "root", "group": group, "mode": mode,
            }));
        } else if md.file_type().is_file() {
            steps.push(serde_json::json!({
                "t": "ensure_file", "file": path, "extsrc": relpath,
                "owner": "root", "group": "root", "mode": mode,
            }));
        } else {
            bail!("unhandled file type at {:?}", ent.path());
        }
    }

    let out = serde_json::to_vec_pretty(&serde_json::json!({
        "steps": steps,
    }))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_template)?;
    f.write_all(&out)?;
    f.flush()?;

    Ok(())
}

struct Publisher {
    name: String,
    origins: Vec<String>,
}

#[derive(Default)]
struct Publishers {
    publishers: Vec<Publisher>,
}

impl Publishers {
    fn has_publisher(&self, publisher: &str) -> bool {
        self.publishers.iter().any(|p| p.name == publisher)
    }

    fn append_origin(&mut self, publisher: &str, url: &str) {
        /*
         * First, make sure the publisher appears in the list of publishers:
         */
        if !self.has_publisher(publisher) {
            self.publishers.push(Publisher {
                name: publisher.to_string(),
                origins: Default::default(),
            });
        }

        /*
         * Add the origin URL to the end of the list for the specified
         * publisher:
         */
        let urls = &mut self
            .publishers
            .iter_mut()
            .find(|p| p.name == publisher)
            .unwrap()
            .origins;

        if !urls.iter().any(|u| u == url) {
            urls.push(url.to_string());
        }
    }

    fn display(&self) -> String {
        let mut s = String::new();

        for p in self.publishers.iter() {
            if !s.is_empty() {
                s += ", ";
            }
            s += &format!("{}={{", p.name);
            for o in p.origins.iter() {
                s += &format!(" {o}");
            }
            if !p.origins.is_empty() {
                s += " ";
            }
            s += "}";
        }

        s
    }
}

fn cmd_image(ca: &CommandArg) -> Result<()> {
    let mut opts = baseopts();
    opts.optflag("d", "", "use DEBUG packages");
    opts.optopt("g", "", "use an external gate directory", "DIR");
    opts.optopt("s", "", "tempdir name suffix", "SUFFIX");
    opts.optopt("o", "", "output directory for image", "DIR");
    opts.optmulti("F", "", "pass extra image builder features", "KEY[=VAL]");
    opts.optflag("B", "", "include omicron1 brand");
    opts.optopt("N", "name", "image name", "NAME");
    opts.optflag("R", "", "recovery image");
    opts.optmulti("X", "", "skip this phase", "PHASE");
    opts.optflag("", "ddr-testing", "build ROMs for other DDR frequencies");
    opts.optmulti(
        "p",
        "",
        "use an external package repository",
        "PUBLISHER=URL",
    );
    opts.optopt("P", "", "include all files from extra proto area", "DIR");
    opts.optmulti(
        "Y",
        "",
        "AMD firmware blob directories override \
        (e.g., \"GN/1.0.0.1\")",
        "DIR",
    );
    opts.optopt("Z", "", "AMD firmware configuration file override", "FILE");

    let usage = || {
        println!(
            "{}",
            opts.usage("Usage: helios [OPTIONS] experiment-image [OPTIONS]")
        );
    };

    let log = ca.log;
    let res = opts.parse(ca.args.iter())?;
    let brand = res.opt_present("B");

    let mut publishers = Publishers::default();
    let local_build = if res.opt_present("p") {
        for arg in res.opt_strs("p") {
            if let Some((key, val)) = arg.split_once('=') {
                if val.trim().is_empty() {
                    bail!("missing url for publisher {key:?}?");
                }

                publishers.append_origin(key, val);
            } else {
                bail!("-p arguments must be PUBLISHER=URL");
            }
        }

        false
    } else {
        /*
         * If no -p argument is provided, we want to use locally built
         * illumos packages.
         */
        true
    };

    let ddr_testing = res.opt_present("ddr-testing");
    let skips = res.opt_strs("X");
    let recovery = res.opt_present("R");

    let extra_proto = if let Some(dir) = res.opt_str("P") {
        let dir = PathBuf::from(dir);
        if !dir.is_dir() {
            bail!("-P must specify a proto area directory");
        }
        Some(dir)
    } else {
        None
    };

    let image_template = res.opt_str("N").unwrap_or_else(|| {
        r"${user}@${host}: ${os_short_commit}; ${date} ${time}".to_string()
    });

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    /*
     * Check that we understand the AMD firmware version and configuration file
     * overrides before we get going.
     */
    let amdconf = if let Some(amdconf) = res.opt_str("Z") {
        /*
         * If the override is an absolute path, just pass it through.  Otherwise
         * look in the place where we usually store the configuration files:
         */
        let f = if amdconf.starts_with("/") {
            let p = PathBuf::from(amdconf);
            assert!(p.is_absolute());
            p
        } else {
            top_path(&["image", "amd", &amdconf])?
        };

        f
    } else {
        /*
         * If there is no override, use the default:
         */
        top_path(&["image", "amd", "milan-gimlet-b.efs.json5"])?
    };
    if !amdconf.is_file() {
        bail!("AMD firmware configuration file {amdconf:?} does not exist?");
    }
    info!(log, "using AMD firmware configuration file {amdconf:?}");

    let amdblobs = if res.opt_present("Y") {
        res.opt_strs("Y")
            .into_iter()
            .map(|y| {
                /*
                 * If the override is an absolute path, just pass it through.
                 * Otherwise look in the place where we usually store the
                 * firmware blobs:
                 */
                let d = if y.starts_with("/") {
                    let p = PathBuf::from(y);
                    assert!(p.is_absolute());
                    p
                } else {
                    top_path(&["projects", "amd-firmware", &y])?
                };

                Ok(d)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        /*
         * If there is no override, use the default:
         */
        vec![top_path(&["projects", "amd-firmware", "GN", "1.0.0.a"])?]
    };
    let missing = amdblobs.iter().filter(|d| !d.is_dir()).collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!("These AMD firmware blob directories do not exist? {missing:?}");
    }
    info!(log, "using AMD firmware blob directories {amdblobs:?}");

    /*
     * Check for the commands we need before we start doing any expensive work.
     */
    let builder = cargo_target_cmd("image-builder", "image-builder", true)?;
    let mkimage = cargo_target_cmd("bootserver", "mkimage", false)?;
    let pinprick = cargo_target_cmd("pinprick", "pinprick", false)?;
    let ahib = cargo_target_cmd(
        "amd-host-image-builder",
        "amd-host-image-builder",
        true,
    )?;
    let baseline = "/usr/lib/brand/omicron1/baseline";
    if brand && !PathBuf::from(baseline).is_file() {
        bail!("Please run: pkg install /system/zones/brand/omicron1/tools");
    }

    /*
     * Make sure the dataset that we want to use for image construction exists.
     */
    let imgds = if let Ok(imgds) = std::env::var("IMAGE_DATASET") {
        imgds
    } else if let Ok(logname) = std::env::var("LOGNAME") {
        format!("rpool/images/{}", logname)
    } else {
        bail!("neither LOGNAME nor IMAGE_DATASET present in environment?");
    };
    if !zfs::dataset_exists(&imgds)? {
        bail!(
            "ZFS dataset {:?} does not exist; we need it to create images",
            imgds
        );
    }
    let mp = zfs::zfs_get(&imgds, "mountpoint")?;

    let gate = if let Some(gate) = res.opt_str("g") {
        abs_path(gate)?
    } else {
        top_path(&["projects", "illumos"])?
    };

    /*
     * We want a temporary directory name that does not overlap with other
     * concurrent usage of this tool.
     */
    let timage = if let Some(suffix) = res.opt_str("s") {
        /*
         * If the user provides a specific suffix, just use it as-is.
         */
        format!("image.{}", suffix)
    } else if let Some(gate) = res.opt_str("g") {
        /*
         * If an external gate is selected, we assume that the base directory
         * name is unique; e.g., "/ws/ftdi", or "/ws/upstream" would yield
         * "ftdi" or "upstream".  This allows repeat runs for the same external
         * workspace to reuse the previous temporary directory.
         */
        format!("image.gate-{}", gate_name(gate)?)
    } else {
        "image".to_string()
    };

    let tempdir = ensure_dir(&["tmp", &timage])?;

    let genproto = {
        let p = rel_path(Some(&tempdir), &["genproto.json"])?;
        if p.exists() {
            /*
             * Remove the old template file to ensure we do not accidentally use
             * a stale copy later.
             */
            std::fs::remove_file(&p)?;
        }

        if let Some(dir) = extra_proto.as_deref() {
            genproto(dir, &p)?;
            info!(log, "generated template {:?} for extra proto {:?}", p, dir);
            Some(p)
        } else {
            None
        }
    };

    if local_build {
        /*
         * In order to install development illumos bits, we first need to elide
         * any files that would conflict with packages delivered from other
         * consolidations.  To do this, we create an onu-specific repository:
         */
        info!(log, "creating temporary repository...");
        let repo = create_transformed_repo(
            log,
            &gate,
            &tempdir,
            res.opt_present("d"),
            false,
        )?;

        publishers
            .append_origin("on-nightly", &format!("file://{}", repo.display()));

        /*
         * For images using locally built illumos packages, include a
         * fallback origin for "helios-dev" as a source for other packages
         * that aren't built locally:
         */
        let relver = determine_release_version()?;
        publishers.append_origin(
            "helios-dev",
            &format!("https://pkg.oxide.computer/helios/{relver}/dev/"),
        );
    } else {
        /*
         * If we have been instructed to use a repository URL, we do not need to
         * do local transformation.  That transformation was done as part of
         * publishing the packages.
         */
        info!(
            log,
            "using external package repositories: {}",
            publishers.display()
        );
    };

    /*
     * The number of unique publishers is currently constrained by the way the
     * template is constructed.  Make sure we are not trying to use more slots
     * than are available:
     */
    const MAXPUBS: usize = 4;
    if publishers.publishers.len() > MAXPUBS {
        bail!(
            "specified {} publishers, but a maximum of {MAXPUBS} are supported",
            publishers.publishers.len(),
        );
    }

    /*
     * Use the image builder to begin creating the image from locally built OS
     * packages, plus other packages from the upstream helios-dev repository.
     */
    let templates = top_path(&["image", "templates"])?;
    let brand_extras = rel_path(Some(&tempdir), &["omicron1"])?;
    let projects_extras = top_path(&["projects"])?;
    std::fs::create_dir_all(&brand_extras)?;
    let basecmd = || -> Command {
        let mut cmd = Command::new("pfexec");
        cmd.arg(&builder);
        cmd.arg("build");
        cmd.arg("-d").arg(&imgds);
        cmd.arg("-g").arg("gimlet");
        cmd.arg("-T").arg(&templates);
        if let Some(genproto) = &genproto {
            cmd.arg("-E").arg(extra_proto.as_deref().unwrap());
            cmd.arg("-F")
                .arg(&format!("genproto={}", genproto.to_str().unwrap()));
        }
        cmd.arg("-E").arg(&brand_extras);
        cmd.arg("-E").arg(&projects_extras);

        assert!(publishers.publishers.len() <= MAXPUBS);
        for (i, p) in publishers.publishers.iter().enumerate() {
            cmd.arg("-F").arg(format!("publisher_{i}_name={}", p.name));
            for o in p.origins.iter() {
                cmd.arg("-F").arg(format!("publisher_{i}_url+={o}"));
            }
        }

        if res.opt_present("d") {
            cmd.arg("-F").arg("debug_variant");
        }
        cmd.arg("-F").arg("baud=3000000");
        if brand {
            cmd.arg("-F").arg("omicron1");
        }
        if recovery {
            cmd.arg("-F").arg("recovery");
        }
        for farg in res.opt_strs("F") {
            cmd.arg("-F").arg(farg);
        }
        cmd
    };

    let root = format!("{}/work/gimlet/ramdisk", mp);
    if !skips.iter().any(|s| s == "install") {
        info!(log, "image builder template: ramdisk-01-os...");
        let mut cmd = basecmd();
        cmd.arg("-n").arg("ramdisk-01-os");
        cmd.arg("--fullreset");
        ensure::run2(log, &mut cmd)?;

        if brand {
            /*
             * After we install packages but before we remove unwanted files
             * from the image (which includes the packaging metadata), we need
             * to generate the baseline archive the omicron1 zone brand uses to
             * populate /etc files.
             */
            info!(log, "omicron1 baseline generation...");

            ensure::run(
                log,
                &[baseline, "-R", &root, &brand_extras.to_str().unwrap()],
            )?;
        }

        info!(log, "image builder template: ramdisk-02-trim...");
        let mut cmd = basecmd();
        cmd.arg("-n").arg("ramdisk-02-trim");
        ensure::run2(log, &mut cmd)?;

        if recovery {
            info!(log, "image builder template: ramdisk-03-recovery-trim...");
            let mut cmd = basecmd();
            cmd.arg("-n").arg("ramdisk-03-recovery-trim");
            ensure::run2(log, &mut cmd)?;
        }
    } else {
        info!(log, "skipping installation phase, using existing archive");
    }

    let tname = if recovery { "zfs-recovery" } else { "zfs" };
    info!(log, "image builder template: {}...", tname);
    let mut cmd = basecmd();
    cmd.arg("-n").arg(tname);
    ensure::run2(log, &mut cmd)?;

    /*
     * Build up the tokens that can be used in the image name.
     */
    let mut tokens = HashMap::new();
    let now: OffsetDateTime = SystemTime::now().into();

    tokens.insert(
        "user".to_string(),
        illumos::get_username()?.unwrap_or_else(|| "unknown".to_string()),
    );
    tokens.insert("host".to_string(), illumos::nodename());
    let dt_fmt = format_description::parse(DATE_FORMAT_STR).unwrap();
    tokens.insert("date".to_string(), now.format(&dt_fmt).unwrap());
    let dt_fmt = format_description::parse(TIME_FORMAT_STR).unwrap();
    tokens.insert("time".to_string(), now.format(&dt_fmt).unwrap());

    let buildfile: PathBuf =
        [&root, "etc", "versions", "build"].iter().collect();
    let hash = match read_string(&buildfile) {
        Ok(s) => {
            info!(log, "BUILD STRING {:?}", s);
            extract_hash(&s).unwrap_or("unknown").to_string()
        }
        _ => "unknown".to_string(),
    };

    tokens.insert("os_short_commit".to_string(), hash);

    let image_name = Expansion::parse(&image_template)?.evaluate(&tokens)?;
    info!(log, "expanded image name: {:?} -> {:?}", image_template, image_name);

    let raw = format!("{}/output/gimlet-{}.raw", mp, tname);

    /*
     * Store built image artefacts together.  Ensure the output directory is
     * empty to begin with.
     */
    let outdir = if let Some(dir) = res.opt_str("o") {
        /*
         * If the user provides an output directory path, use it uncritically:
         */
        PathBuf::from(dir)
    } else {
        /*
         * Otherwise, make one relative to the repository:
         */
        top_path(&["image", "output"])?
    };
    if exists_dir(&outdir)? {
        std::fs::remove_dir_all(&outdir)?;
    }
    std::fs::create_dir(&outdir)?;
    info!(log, "output artefacts stored in: {:?}", outdir);

    /*
     * Assemble a set of extra metadata to include in the archive.
     */
    let mut infos = vec![(
        "image-args.txt".to_string(),
        format!("image arguments: {:#?}\n", ca.args).as_bytes().to_vec(),
    )];

    /*
     * Include some basic git metadata from the set of project directories we
     * have cloned locally and are using as part of building this image.
     */
    {
        let projdir = top_path(&["projects"])?;
        let mut wd = std::fs::read_dir(&projdir)?;

        while let Some(ent) = wd.next().transpose()? {
            let dir = ent.path();
            if !dir.is_dir() {
                bail!("unexpected item in project area: {:?}", ent.path());
            }
            let name = dir.file_name().unwrap().to_str().unwrap().to_string();

            info!(log, "collecting git info from project {name:?}...");

            let mut cmd = Command::new("git");
            cmd.env_clear();
            cmd.arg("status");
            cmd.arg("-b");
            cmd.arg("--porcelain=2");
            cmd.current_dir(&dir);

            let out = cmd.output()?;
            if !out.status.success() {
                bail!("could not git status in {:?}: {}", dir, out.info());
            }
            let data = String::from_utf8(out.stdout)?.as_bytes().to_vec();

            infos.push((format!("git-status-{}.txt", name), data));
        }
    }

    /*
     * We want to include a full list of all of the packages that were installed
     * into the image prior to any trimming of individual files.  This will make
     * it easier to tell exactly what files went into a particular image, and
     * will allow us to more accurately reproduce the same image later by using
     * the same packages.
     */
    let pkg_infos = [
        ("pkg-publishers.txt", ["publisher", "-F", "tsv"]),
        ("pkg-list.txt", ["list", "-H", "-v"]),
    ];
    /*
     * Because we have already stripped the packaging metadata out of the final
     * image, go back to using the snapshot that is created at the end of the
     * "ramdisk-01-os" step:
     */
    let snapdir = rel_path(Some(&root), &[".zfs", "snapshot", "os"])?;
    for (name, args) in pkg_infos {
        info!(log, "collecting packaging info {name:?}: {args:?}...");

        let mut cmd = Command::new("pfexec");
        cmd.env_clear();
        cmd.arg("pkg");
        cmd.arg("-R").arg(&snapdir);
        for a in args {
            cmd.arg(a);
        }

        let out = cmd.output()?;
        if !out.status.success() {
            bail!("could not run {args:?} into {name:?}: {}", out.info());
        }
        let data = String::from_utf8(out.stdout)?.as_bytes().to_vec();

        infos.push((name.to_string(), data));
    }

    /*
     * Oxide boot images need a header that contains some basic metadata like
     * the SHA256 hash of the image itself.  This header is consumed by the
     * kernel boot code when reading the image from an NVMe device, and by the
     * network boot server.
     */
    let zfsimg = rel_path(Some(&outdir), &["zfs.img"])?;

    /*
     * The CPIO archive also needs to know the image checksum so that we can
     * boot only a matching ramdisk image.
     */
    let csumfile = rel_path(Some(&tempdir), &["boot_image_csum"])?;

    /*
     * Create the image and extract the checksum:
     */
    let target_size = 4 * 1024;
    info!(log, "creating Oxide boot image...");
    let mut cmd = Command::new(&mkimage);
    cmd.arg("-i").arg(&raw);
    cmd.arg("-N").arg(&image_name);
    cmd.arg("-o").arg(zfsimg.to_str().unwrap());
    cmd.arg("-O").arg(csumfile.to_str().unwrap());
    cmd.arg("-s").arg(&target_size.to_string());
    if recovery {
        cmd.arg("-z");
    }
    ensure::run2(log, &mut cmd)?;

    /*
     * Read the image checksum back in from the file that was built for
     * inclusion in the boot archive.  The file format is the raw bytes of the
     * hash rather than ASCII hexadecimal, so we must reformat it for inclusion
     * in the archive metadata as a string.
     */
    let csum = std::fs::File::open(&csumfile)?
        .bytes()
        .map(|b| Ok(format!("{:02x}", b?)))
        .collect::<Result<String>>()?;

    /*
     * Begin creating the archive now so that the archiver worker thread can
     * begin compressing it while we are doing other things.
     */
    let tarpath = rel_path(Some(&outdir), &["os.tar.gz"])?;
    let tar = archive::Archive::new(
        &tarpath,
        metadata::MetadataBuilder::new(ArchiveType::Os)
            .info("name", &image_name)?
            .info("checksum", &csum)?
            .build()?,
    )?;

    for (name, data) in infos {
        tar.add_file_with_data(data, &name)?;
    }

    tar.add_file(&zfsimg, "zfs.img")?;

    /*
     * Create the boot archive (CPIO) with the kernel and modules that we need
     * to boot and mount the ramdisk.
     * XXX This should be an image-builder feature.
     */
    let mkcpio = top_path(&["image", "mkcpio.sh"])?;
    let cpio = rel_path(Some(&outdir), &["cpio"])?;
    info!(log, "creating boot archive (CPIO)...");
    ensure::run(
        log,
        &[
            "bash",
            mkcpio.to_str().unwrap(),
            &root,
            cpio.to_str().unwrap(),
            tempdir.to_str().unwrap(),
        ],
    )?;

    /*
     * Create a compressed cpio archive and kernel suitable for passing
     * to a development loader over the UART.  These will also be included
     * in the archive, in case they are required for engineering
     * activities later.
     */
    let cpioz = rel_path(Some(&outdir), &["cpio.z"])?;
    let unix = format!("{}/platform/oxide/kernel/amd64/unix", root);
    let unixz = rel_path(Some(&outdir), &["unix.z"])?;
    info!(log, "creating compressed cpio/unix for dev loaders...");
    ensure::run(
        log,
        &[
            "bash",
            "-c",
            &format!(
                "'{}' '{}' >'{}'",
                pinprick,
                unix,
                unixz.to_str().unwrap()
            ),
        ],
    )?;
    tar.add_file(&unixz, "unix.z")?;
    ensure::run(
        log,
        &[
            "bash",
            "-c",
            &format!(
                "'{}' '{}' >'{}'",
                pinprick,
                cpio.to_str().unwrap(),
                cpioz.to_str().unwrap()
            ),
        ],
    )?;
    tar.add_file(&cpioz, "cpio.z")?;

    /*
     * Create the reset image for the Gimlet SPI ROM:
     */
    info!(log, "creating reset image...");
    let phbl_path = top_path(&["projects", "phbl"])?;
    rustup_install_toolchain(log, &phbl_path)?;
    ensure::run_in(
        log,
        &phbl_path,
        &[
            "cargo",
            "xtask",
            "build",
            "--release",
            "--cpioz",
            cpioz.to_str().unwrap(),
        ],
    )?;
    info!(log, "building host image...");
    let rom = rel_path(Some(&outdir), &["rom"])?;
    let reset = top_path(&[
        "projects",
        "phbl",
        "target",
        "x86_64-oxide-none-elf",
        "release",
        "phbl",
    ])?;
    let ahibdir = top_path(&["projects", "amd-host-image-builder"])?;
    let ahibargs_base = {
        let mut t: Vec<String> = vec![ahib.as_str().into()];
        for blob in amdblobs {
            t.push("-B".into());
            t.push(blob.to_str().unwrap().into());
        }
        t
    };
    let ahibargs = {
        let mut t = ahibargs_base.clone();

        t.push("--config".into());
        t.push(amdconf.to_str().unwrap().into());

        t.push("--output-file".into());
        t.push(rom.to_str().unwrap().into());

        t.push("--reset-image".into());
        t.push(reset.to_str().unwrap().into());

        t
    };
    ensure::run_in(
        log,
        &ahibdir,
        &ahibargs.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    tar.add_file(&rom, "rom")?;

    if ddr_testing {
        /*
         * The configuration for amd-host-image-builder is stored in JSON5
         * format.  Read the file as a generic JSON object:
         */
        let f = std::fs::read_to_string(&amdconf)?;
        let inputcfg: serde_json::Value = json5::from_str(&f)?;

        for limit in [1600, 1866, 2133, 2400, 2667, 2933, 3200] {
            let romname = format!("rom.ddr{limit}");
            let rom = rel_path(Some(&outdir), &[&romname])?;

            /*
             * Produce a new configuration file with the specified
             * MemBusFrequencyLimit:
             */
            let tmpcfg = rel_path(
                Some(&tempdir),
                &[&format!("milan-gimlet-b.ddr{}.efs.json", limit)],
            )?;
            maybe_unlink(&tmpcfg)?;
            mk_rom_config(inputcfg.clone(), &tmpcfg, limit)?;

            /*
             * Build the frequency-specific ROM file for this frequency limit:
             */
            let ahibargs = {
                let mut t = ahibargs_base.clone();

                t.push("--config".into());
                t.push(tmpcfg.to_str().unwrap().into());

                t.push("--output-file".into());
                t.push(rom.to_str().unwrap().into());

                t.push("--reset-image".into());
                t.push(reset.to_str().unwrap().into());

                t
            };
            ensure::run_in(
                log,
                &ahibdir,
                &ahibargs.iter().map(String::as_str).collect::<Vec<_>>(),
            )?;
            tar.add_file(&rom, &romname)?;
        }
    }

    info!(log, "finishing image archive at {tarpath:?}...");
    tar.finish()?;

    info!(log, "image complete! materials are in {:?}", outdir);
    std::fs::remove_dir_all(&tempdir).ok();
    Ok(())
}

fn mk_rom_config(
    mut input: serde_json::Value,
    output: &Path,
    ddr_speed: u32,
) -> Result<()> {
    let Some(bhd) = input.get_mut("bhd") else {
        bail!("could not find bhd");
    };
    let Some(dir) = bhd.get_mut("BhdDirectory") else {
        bail!("could not find BhdDirectory");
    };
    let Some(entries) = dir.get_mut("entries") else {
        bail!("could not find entries");
    };
    let Some(entries) = entries.as_array_mut() else {
        bail!("entries is not an array");
    };

    for e in entries.iter_mut() {
        #[derive(Deserialize)]
        struct EntryTarget {
            #[serde(rename = "type")]
            type_: String,
        }

        #[derive(Deserialize)]
        struct Entry {
            target: EntryTarget,
        }

        let ee: Entry = serde_json::from_value(e.clone())?;
        if ee.target.type_ != "ApcbBackup" {
            continue;
        }

        let Some(src) = e.get_mut("source") else {
            bail!("could not find source");
        };
        let Some(apcb) = src.get_mut("ApcbJson") else {
            bail!("could not find ApcbJson");
        };
        let Some(entries) = apcb.get_mut("entries") else {
            bail!("could not find entries");
        };
        let Some(entries) = entries.as_array_mut() else {
            bail!("entries is not an array");
        };

        for e in entries.iter_mut() {
            #[derive(Deserialize)]
            struct Header {
                group_id: u32,
                entry_id: u32,
                instance_id: u32,
            }

            let Some(h) = e.get("header") else {
                bail!("could not find header");
            };
            let h: Header = serde_json::from_value(h.clone())?;

            if h.group_id != 0x3000
                || h.entry_id != 0x0004
                || h.instance_id != 0
            {
                continue;
            }

            let Some(tokens) = e.get_mut("tokens") else {
                bail!("could not get tokens");
            };
            let Some(tokens) = tokens.as_array_mut() else {
                bail!("tokens is not an array");
            };

            for t in tokens.iter_mut() {
                let Some(dword) = t.get_mut("Dword") else {
                    continue;
                };
                let Some(dword) = dword.as_object_mut() else {
                    continue;
                };
                {
                    let keys = dword.keys().collect::<Vec<_>>();
                    if keys.len() != 1 {
                        bail!("too many keys? {:?}", keys);
                    }
                    if keys[0] != "MemBusFrequencyLimit" {
                        continue;
                    }
                }
                dword.insert(
                    "MemBusFrequencyLimit".to_string(),
                    serde_json::Value::String(format!("Ddr{}", ddr_speed)),
                );
            }
        }
    }

    /*
     * Write the file to the target path as JSON.  Note that we are not using
     * JSON5 here, but that ostensibly doesn't matter as JSON5 is a superset of
     * JSON.
     */
    let s = serde_json::to_string_pretty(&input)?;
    std::fs::write(output, &s)?;

    Ok(())
}

/**
 * When we respin a release build with a backport, we first create a branch that
 * is a child of the original commit we used from the main branch; e.g.,
 * "rel/v12" is a branch from some point in the history of "stlouis" with one
 * extra commit).  Use "git merge-base" to find a common ancestor commit; i.e.,
 * one which appears both in the specified parent branch, and in the history of
 * the nominated commit.
 */
fn git_branch_point<P: AsRef<Path>>(
    path: P,
    parent_branch: &str,
    commit: &str,
) -> Result<String> {
    let out = Command::new("git")
        .env_clear()
        .arg("merge-base")
        .arg(parent_branch)
        .arg(commit)
        .current_dir(path.as_ref())
        .output()?;

    if !out.status.success() {
        bail!(
            "git merge-base ({parent_branch:?}, {commit:?}) failed: {}",
            out.info()
        );
    }

    let res = String::from_utf8(out.stdout)?;
    Ok(res.trim().parse()?)
}

/**
 * Count commits.  If a branch or commit ID is provided, the count of commits
 * will be from the beginning of the repository up to the named reference; e.g.,
 * "HEAD" is a common argument.  One can also specify a range, and the count
 * will be from the first point to the second point; e.g., "stlouis..HEAD".
 */
fn git_commit_count<P: AsRef<Path>>(path: P, commit: &str) -> Result<u32> {
    let out = Command::new("git")
        .env_clear()
        .arg("rev-list")
        .arg("--count")
        .arg(commit)
        .current_dir(path.as_ref())
        .output()?;

    if !out.status.success() {
        bail!("git commit count ({commit:?}) failed: {}", out.info());
    }

    let res = String::from_utf8(out.stdout)?;
    Ok(res.trim().parse()?)
}

struct BranchStatus {
    oid: String,
    head: String,
}

fn git_branch_status<P: AsRef<Path>>(path: P) -> Result<BranchStatus> {
    let out = Command::new("git")
        .env_clear()
        .arg("status")
        .arg("--branch")
        .arg("--porcelain=v2")
        .current_dir(path.as_ref())
        .output()?;

    if !out.status.success() {
        bail!("git branch status failed: {}", out.info());
    }

    let res = String::from_utf8(out.stdout)?;

    let mut oid = None;
    let mut head = None;
    for l in res.lines() {
        let t = l.split_ascii_whitespace().collect::<Vec<_>>();
        if t.len() < 3 || t[0] != "#" {
            continue;
        }

        match t[1] {
            "branch.oid" => {
                if t.len() != 3 {
                    bail!("unexpected branch.oid line: {t:?}");
                }

                oid = Some(t[2].to_string());
            }
            "branch.head" => {
                if t.len() != 3 {
                    bail!("unexpected branch.head line: {t:?}");
                }

                head = Some(t[2].to_string());
            }
            _ => (),
        }
    }

    if let Some((oid, head)) = oid.zip(head) {
        Ok(BranchStatus { oid, head })
    } else {
        bail!("oid or head missing from branch status? {res:?}");
    }
}

fn cmd_setup(ca: &CommandArg) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] setup [OPTIONS]"));
    };

    let log = ca.log;
    let res = opts.parse(ca.args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    let relver = determine_release_version()?;

    let top = top()?;
    info!(log, "helios repository root is: {}", top.display());

    /*
     * Read the projects file which contains the URLs of the repositories we
     * need to clone.
     */
    let p: Projects = read_toml(top_path(&["config", "projects.toml"])?)?;

    ensure_dir(&["projects"])?;
    ensure_dir(&["tmp"])?;

    for (name, project) in p.project.iter() {
        let path = top_path(&["projects", &name])?;
        let url = project.url(false)?;
        let tmp = ensure_dir(&["tmp", &name])?;

        if let Some(reason) = project.skip_reason() {
            info!(log, "skipping project {name:?} because {reason}");
            continue;
        }

        let log = log.new(o!("project" => name.to_string()));
        info!(log, "project {name}: {project:?}");

        if exists_dir(&path)? {
            info!(log, "clone {url} exists already at {path:?}");
            if project.auto_update {
                info!(log, "fetching updates for clone ...");
                let mut child = if let Some(rev) = &project.rev {
                    Command::new("git")
                        .current_dir(&path)
                        .arg("fetch")
                        .arg("origin")
                        .arg(rev)
                        .spawn()?
                } else {
                    Command::new("git")
                        .current_dir(&path)
                        .arg("fetch")
                        .spawn()?
                };

                let exit = child.wait()?;
                if !exit.success() {
                    bail!("fetch in {} failed", path.display());
                }

                /*
                 * Apply fixups to avoid the need for manual flag days in some
                 * cases.
                 */
                for fixup in &project.fixup {
                    let bs = git_branch_status(&path)?;

                    if &bs.head == "(detached)" && bs.oid == fixup.from_commit {
                        info!(
                            log,
                            "applying fixup: moving to branch {}...",
                            fixup.to_branch
                        );
                        let mut child = Command::new("git")
                            .current_dir(&path)
                            .arg("checkout")
                            .arg(&fixup.to_branch)
                            .spawn()?;

                        let exit = child.wait()?;
                        if !exit.success() {
                            bail!("branch switch in {} failed", path.display());
                        }
                    }
                }

                if let Some(rev) = &project.rev {
                    info!(log, "pinning to revision {rev}...");
                    let mut child = Command::new("git")
                        .current_dir(&path)
                        .arg("checkout")
                        .arg(rev)
                        .spawn()?;

                    let exit = child.wait()?;
                    if !exit.success() {
                        bail!("update merge in {} failed", path.display());
                    }
                } else {
                    info!(log, "rolling branch forward...");
                    let mut child = Command::new("git")
                        .current_dir(&path)
                        .arg("merge")
                        .arg("--ff-only")
                        .spawn()?;

                    let exit = child.wait()?;
                    if !exit.success() {
                        bail!("update merge in {} failed", path.display());
                    }
                }

                info!(log, "updating submodules...");
                let mut child = Command::new("git")
                    .current_dir(&path)
                    .arg("submodule")
                    .arg("update")
                    .arg("--recursive")
                    .spawn()?;

                let exit = child.wait()?;
                if !exit.success() {
                    bail!("submodule update in {} failed", path.display());
                }
            }
        } else {
            info!(log, "cloning {url} at {path:?}...");
            let mut child = Command::new("git")
                .arg("clone")
                .arg("--recurse-submodules")
                .arg(&url)
                .arg(&path)
                .spawn()?;

            let exit = child.wait()?;
            if !exit.success() {
                bail!("clone of {} to {} failed", url, path.display());
            }

            if let Some(rev) = &project.rev {
                info!(log, "fetching revision {rev} for clone ...");
                let mut child = Command::new("git")
                    .current_dir(&path)
                    .arg("fetch")
                    .arg("origin")
                    .arg(rev)
                    .spawn()?;

                let exit = child.wait()?;
                if !exit.success() {
                    bail!("fetch in {} failed", path.display());
                }

                info!(log, "pinning to revision {rev}...");
                let mut child = Command::new("git")
                    .current_dir(&path)
                    .arg("checkout")
                    .arg(rev)
                    .spawn()?;

                let exit = child.wait()?;
                if !exit.success() {
                    bail!("update merge in {} failed", path.display());
                }

                info!(log, "updating submodules...");
                let mut child = Command::new("git")
                    .current_dir(&path)
                    .arg("submodule")
                    .arg("update")
                    .arg("--recursive")
                    .spawn()?;

                let exit = child.wait()?;
                if !exit.success() {
                    bail!("submodule update in {} failed", path.display());
                }
            }

            info!(log, "clone ok!");
        }

        if project.site_sh {
            let mut ssp = path.clone();
            ssp.push("lib");
            ssp.push("site.sh");
            info!(log, "creating config file at {}", ssp.display());

            let mut site_sh = String::new();
            site_sh += "PFEXEC=/usr/bin/pfexec\n";
            site_sh += "PKGPUBLISHER=helios-dev\n";
            site_sh += "HOMEURL=https://oxide.computer/helios\n";
            site_sh += "PUBLISHER_EMAIL=jmc@oxide.computer\n";
            site_sh += &format!("RELVER={}\n", relver);
            site_sh += &format!("DASHREV={}\n", DASHREV);
            site_sh += "PVER=$RELVER.$DASHREV\n";
            site_sh += "IPS_REPO=https://pkg.oxide.computer/helios/2/dev\n";
            site_sh += &format!("TMPDIR={}\n", &tmp.to_str().unwrap());
            site_sh += "DTMPDIR=$TMPDIR\n";

            ensure::file_str(
                &log,
                &site_sh,
                &ssp,
                0o644,
                ensure::Create::Always,
            )?;
        }

        if name == "illumos" {
            /*
             * When doing initial setup, we don't care about the potential for a
             * parent branch for versioning purposes.  The actual build of the
             * branch must be done with the "-b" argument, which will result in
             * new and correct environment files.
             */
            let br = None;

            regen_illumos_sh(&log, &path, BuildType::Full, relver, &br)?;
            regen_illumos_sh(&log, &path, BuildType::QuickDebug, relver, &br)?;
            regen_illumos_sh(&log, &path, BuildType::Quick, relver, &br)?;
            regen_illumos_sh(&log, &path, BuildType::Release, relver, &br)?;
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

    regen_publisher_mog(log, NO_PATH, publisher)?;
    for mog in &["os-conflicts", "os-deps"] {
        let mogpath = top_path(&["packages", &format!("{}.mogrify", mog)])?;
        ensure::symlink(
            log,
            &mogpath,
            &format!("../tools/packages/{}.mogrify", mog),
        )?;
    }

    /*
     * Perform setup in project repositories that require it.
     */
    for (name, project) in p.project.iter().filter(|p| p.1.cargo_build) {
        if project.skip() {
            continue;
        }

        let path = top_path(&["projects", &name])?;
        rustup_install_toolchain(log, &path)?;

        info!(log, "building project {:?} at {}", name, path.display());
        let start = Instant::now();
        let mut args = vec!["cargo", "build", "--locked"];
        if !project.use_debug {
            args.push("--release");
        }
        ensure::run_in(log, &path, &args)?;
        let delta = Instant::now().saturating_duration_since(start).as_secs();
        info!(log, "building project {:?} ok ({} seconds)", name, delta);
    }

    Ok(())
}

struct CommandArg<'a> {
    log: &'a Logger,
    args: &'a [&'a str],
}

struct CommandInfo {
    name: String,
    desc: String,
    func: fn(&CommandArg) -> Result<()>,
    hide: bool,
    blank: bool,
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
        blank: false,
    });
    handlers.push(CommandInfo {
        name: "genenv".into(),
        desc: "generate environment file for illumos build".into(),
        func: cmd_illumos_genenv,
        hide: false,
        blank: true,
    });
    handlers.push(CommandInfo {
        name: "bldenv".into(),
        desc: "enter a bldenv shell for illumos so you can run dmake".into(),
        func: cmd_illumos_bldenv,
        hide: false,
        blank: true,
    });
    handlers.push(CommandInfo {
        name: "onu".into(),
        desc: "install your non-DEBUG build of illumos on this system".into(),
        func: cmd_illumos_onu,
        hide: false,
        blank: false,
    });
    handlers.push(CommandInfo {
        name: "build-illumos".into(),
        desc: "run a full nightly(1) and produce packages".into(),
        func: cmd_build_illumos,
        hide: false,
        blank: false,
    });
    handlers.push(CommandInfo {
        name: "merge-illumos".into(),
        desc: "merge DEBUG and non-DEBUG packages into one repository".into(),
        func: cmd_merge_illumos,
        hide: false,
        blank: false,
    });
    handlers.push(CommandInfo {
        name: "experiment-image".into(),
        desc: "experimental image construction for Gimlets".into(),
        func: cmd_image,
        hide: false,
        blank: false,
    });
    handlers.push(CommandInfo {
        name: "image".into(),
        desc: "experimental image construction for Gimlets".into(),
        func: cmd_image,
        hide: true,
        blank: false,
    });
    handlers.push(CommandInfo {
        name: "help".into(),
        desc: "display usage information".into(),
        /*
         * No behaviour is required here.  The "help" command is a special case
         * in the argument processing below.
         */
        func: |_: &CommandArg| Ok(()),
        hide: false,
        blank: true,
    });

    let usage = |failure: bool| {
        let mut out = String::new();
        out += "Usage: helios [OPTIONS] COMMAND [OPTIONS] [ARGS...]\n\n";
        for ci in handlers.iter() {
            if ci.hide {
                continue;
            }

            if ci.blank {
                out += "\n";
            }

            out += &format!("    {:<16} {}\n", ci.name, ci.desc);
        }
        let msg = opts.usage(&out);
        if failure {
            eprintln!("{msg}");
        } else {
            println!("{msg}");
        }
    };

    let res = match opts.parse(std::env::args_os().skip(1)) {
        Ok(res) => res,
        Err(e) => {
            usage(true);
            bail!("{e}");
        }
    };

    if res.opt_present("help") {
        usage(false);
        return Ok(());
    }

    if res.free.is_empty() {
        usage(true);
        bail!("choose a command");
    }

    if res.free[0] == "help" {
        usage(false);
        return Ok(());
    }

    let args = res.free[1..].iter().map(|s| s.as_str()).collect::<Vec<_>>();

    let log = init_log();

    for ci in handlers.iter() {
        if ci.name != res.free[0] {
            continue;
        }

        let ca = CommandArg { log: &log, args: args.as_slice() };

        return (ci.func)(&ca);
    }

    usage(true);
    bail!("command \"{}\" not understood", res.free[0]);
}

/*
 * Extract a hash from a string produced by:
 *    git describe --all --long --dirty
 */
fn extract_hash(s: &str) -> Option<&str> {
    /*
     * Look from the end for a column that appears to be the git hash.
     */
    s.split('-').rev().find_map(|col| {
        /*
         * Require column that starts with `g`; strip it off.
         */
        let suffix = col.strip_prefix('g')?;

        /*
         * Require suffix to be at least 7 chars and all ascii.
         */
        if suffix.len() < 7 || !suffix.is_ascii() {
            return None;
        }

        /*
         * We know "suffix" is ascii, so slicing is safe (we can't land
         * mid-UTF8 codepoint).
         */
        let suffix = &suffix[..7];

        /*
         * Cheap hack to check for all hex: try to parse this as a integer.
         * We've already trimmed to at most 7 chars, so u32 is big enough.
         */
        if u32::from_str_radix(suffix, 16).is_ok() {
            Some(suffix)
        } else {
            None
        }
    })
}

fn rustup_install_toolchain<P: AsRef<Path>>(log: &Logger, p: P) -> Result<()> {
    let p = p.as_ref();

    /*
     * rustup 1.28.0 removed the long-standing default behavior of automatically
     * installing toolchains for projects.  It also introduces the ability to
     * call "rustup toolchain install" with no argument to automatically install
     * the current toolchain.  Of course, this does not exist in earlier
     * releases, and there was no transition period.
     *
     * "rustup show active-toolchain || rustup toolchain install" is the
     * recommended way to just install the toolchain regardless of rustup
     * version.
     */
    info!(log, "checking rust toolchain is installed for {p:?}");
    let out = Command::new("rustup")
        .args(["show", "active-toolchain"])
        .current_dir(p)
        .output()?;

    if out.status.success() {
        let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
        info!(log, "rust toolchain for {p:?}: {ver:?}");
    } else {
        info!(log, "installing rust toolchain for {p:?}...");
        ensure::run_in(log, p, &["rustup", "toolchain", "install"])?;
    }

    Ok(())
}

#[test]
fn hash_extract() {
    assert_eq!(extract_hash("heads/trim-0-g49fb31d-dirty"), Some("49fb31d"));
    assert_eq!(extract_hash("heads/r151046-0-g82ebda23c9"), Some("82ebda2"));
    assert_eq!(extract_hash("heads/master-0-g77f745e"), Some("77f745e"));
    assert_eq!(extract_hash("heads/master-0-g7f745e"), None);
    assert_eq!(extract_hash("heads/master-0"), None);
}
