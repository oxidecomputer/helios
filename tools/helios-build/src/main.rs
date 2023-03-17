mod common;
use common::*;

use anyhow::{Result, Context, bail};
use helios_build_utils::metadata::{ArchiveType, self};
use serde::Deserialize;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::os::unix::process::CommandExt;
use std::io::{BufReader, Read, Write};
use std::fs::File;
use std::time::{Instant,SystemTime};
use slog::Logger;
use std::path::Path;
use time::{format_description, OffsetDateTime};
use walkdir::{WalkDir, DirEntry};
use regex::Regex;
use helios_build_utils::tree;

pub mod illumos;
pub mod ensure;
mod zfs;
mod archive;

const PKGREPO: &str = "/usr/bin/pkgrepo";
const PKGRECV: &str = "/usr/bin/pkgrecv";
const PKGDEPOTD: &str = "/usr/lib/pkg.depotd";

const RELVER: u32 = 1;
const DASHREV: u32 = 0;

const DATE_FORMAT_STR: &'static str =
    "[year]-[month]-[day] [hour]:[minute]:[second]";

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
    let mut top = if let Some(p) = p {
        p.as_ref().to_path_buf()
    } else {
        top()?
    };
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
     * When cloning or updating this repository, pin to this commit hash:
     */
    #[serde(default)]
    commit: Option<String>,

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
        println!("{}",
            opts.usage("Usage: helios [OPTIONS] merge-illumos [OPTIONS]"));
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

    let repo_d = rel_path(Some(&gate),
        &["packages", "i386", "nightly", "repo.redist"])?;
    let repo_nd = rel_path(Some(&gate),
        &["packages", "i386", "nightly-nd", "repo.redist"])?;

    /*
     * Merge the packages from the DEBUG and non-DEBUG builds into a single
     * staging repository using the IPS variant feature.
     */
    info!(log, "recreating merging repository at {:?}", &repo_merge);
    create_ips_repo(log, &repo_merge, &input_publisher, true)?;

    ensure::run(log, &["/usr/bin/pkgmerge", "-d", &repo_merge.to_str().unwrap(),
        "-s", &format!("debug.illumos=false,{}/", repo_nd.to_str().unwrap()),
        "-s", &format!("debug.illumos=true,{}/", repo_d.to_str().unwrap())])?;

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

    ensure::run(log, &[PKGRECV,
        "-s", &repo_merge.to_str().unwrap(),
        "-d", &repo.to_str().unwrap(),
        "--mog-file", &mog_publisher.to_str().unwrap(),
        "--mog-file", &mog_conflicts.to_str().unwrap(),
        "--mog-file", &mog_deps.to_str().unwrap(),
        "-m", "latest",
        "*"])?;
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

fn regen_publisher_mog<P: AsRef<Path>>(log: &Logger, mogfile: Option<P>,
    publisher: &str) -> Result<()>
{
    /*
     * Create the pkgmogrify template that we need to replace the pkg(5)
     * publisher name when promoting packages from a build repository to the
     * central repository.
     */
    let mog = format!("<transform set name=pkg.fmri -> \
        edit value pkg://[^/]+/ pkg://{}/>\n", publisher);
    let mogpath = if let Some(mogfile) = mogfile {
        mogfile.as_ref().to_path_buf()
    } else {
        top_path(&["packages", "publisher.mogrify"])?
    };
    ensure::file_str(log, &mog, &mogpath, 0o644, ensure::Create::Always)?;
    Ok(())
}

fn regen_illumos_sh<P: AsRef<Path>>(log: &Logger, gate: P, bt: BuildType)
    -> Result<PathBuf>
{
    let gate = gate.as_ref();
    let path_env = rel_path(Some(gate), &[bt.script_name()])?;

    let maxjobs = ncpus()?;

    let (rnum, vers, banner) = match bt {
        /*
         * Though git does not support an SVN- or Mercurial-like revision
         * number, our history is sufficiently linear that we can approximate
         * one anyway.  Use that to set an additional version number component
         * beyond the release version, and as the value for "uname -v":
         */
        BuildType::Release => {
            let rnum = git_commit_count(&gate)?;
            let vers = format!("helios-{}.{}.{}", RELVER, DASHREV, rnum);
            (rnum, vers, "Oxide Helios Version ^v ^w-bit")
        }
        /*
         * If this is a quick build that one intends to install on the local
         * system and iterate on, set the revision number to an extremely high
         * number that is obviously not related to the production package commit
         * numbers:
         */
        BuildType::Quick | BuildType::QuickDebug | BuildType::Full => {
            let vers = "$(git describe --long --all HEAD | cut -d/ -f2-)";
            (999999, vers.into(), "Oxide Helios Version ^v ^w-bit (onu)")
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
    env += "export GNUC_ROOT=/opt/gcc-7\n";
    env += "export PRIMARY_CC=gcc7,$GNUC_ROOT/bin/gcc,gnu\n";
    env += "export PRIMARY_CCC=gcc7,$GNUC_ROOT/bin/g++,gnu\n";
    match bt {
        BuildType::Quick | BuildType::QuickDebug => {
            /*
             * Skip the shadow compiler and smatch for quick builds:
             */
            env += "export SHADOW_CCS=\n";
            env += "export SHADOW_CCCS=\n";
        }
        BuildType::Full | BuildType::Release => {
            /*
             * Enable the shadow compiler for full builds:
             */
            env += "export SHADOW_CCS=gcc10,/opt/gcc-10/bin/gcc,gnu\n";
            env += "export SHADOW_CCCS=gcc10,/opt/gcc-10/bin/g++,gnu\n";

            /*
             * Enable smatch checks for full builds:
             */
            env += "export SMATCHBIN=$CODEMGR_WS/usr/src/tools/proto/\
                root_$MACH-nd/opt/onbld/bin/$MACH/smatch\n";
            env += "export SHADOW_CCS=\"$SHADOW_CCS \
                smatch,$SMATCHBIN,smatch\"\n";
        }
    }
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
    env += "export BUILDPY2='#'\n";
    env += "export BUILDPY3=\n";
    env += "export BUILDPY2TOOLS='#'\n";
    env += "export BUILDPY3TOOLS=\n";
    env += "export PYTHON3_VERSION=3.9\n";
    env += "export PYTHON3_PKGVERS=-39\n";
    env += "export PYTHON3_SUFFIX=\n";
    env += "export TOOLS_PYTHON=/usr/bin/python$PYTHON3_VERSION\n";
    env += "export STAFFER=\"$LOGNAME\"\n";
    env += "export MAILTO=\"${MAILTO:-$STAFFER}\"\n";
    env += "export BUILD_PROJECT=''\n";
    env += "export ATLOG=\"$CODEMGR_WS/log\"\n";
    env += "export LOGFILE=\"$ATLOG/nightly.log\"\n";
    env += "export BUILD_TOOLS='/opt'\n";
    env += "export MAKEFLAGS='k'\n";
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
    env += &format!("export PKGVERS_BRANCH={}.{}.{}\n", RELVER, DASHREV, rnum);

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

    let usage = || {
        println!("{}",
            opts.usage("Usage: helios [OPTIONS] build-illumos [OPTIONS]"));
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

    let gate = if let Some(gate) = res.opt_str("g") {
        abs_path(gate)?
    } else {
        top_path(&["projects", "illumos"])?
    };
    let env_sh = regen_illumos_sh(log, &gate, bt)?;

    let script = format!("cd {} && ./usr/src/tools/scripts/nightly{} {}",
        gate.to_str().unwrap(),
        if res.opt_present("i") { " -i" } else { "" },
        env_sh.to_str().unwrap());

    ensure::run(log, &["/sbin/sh", "-c", &script])?;

    Ok(())
}

fn create_transformed_repo(log: &Logger, gate: &Path, tmpdir: &Path,
    debug: bool, refresh: bool)
    -> Result<PathBuf>
{
    let repo = rel_path(Some(tmpdir), &["repo.redist"])?;
    create_ips_repo(log, &repo, "on-nightly", true)?;

    /*
     * These pkgmogrify(1) scripts will drop any conflicting actions:
     */
    let mog_conflicts = top_path(&["packages", "os-conflicts.mogrify"])?;
    let mog_deps = top_path(&["packages", "os-deps.mogrify"])?;

    info!(log, "transforming packages for installation...");
    let which = if debug { "nightly" } else { "nightly-nd" };
    let repo_nd = rel_path(Some(gate),
        &["packages", "i386", which, "repo.redist"])?;
    ensure::run(log, &[PKGRECV,
        "-s", &repo_nd.to_str().unwrap(),
        "-d", &repo.to_str().unwrap(),
        "--mog-file", &mog_conflicts.to_str().unwrap(),
        "--mog-file", &mog_deps.to_str().unwrap(),
        "-m", "latest",
        "*"])?;
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
    let repo = create_transformed_repo(log, &gate,
        &ensure_dir(&["tmp", &tonu])?, res.opt_present("d"), true)?;

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
    let onu = top_path(&["projects", "illumos", "usr", "src",
        "tools", "proto", "root_i386-nd", "opt", "onbld", "bin", "onu"])?;
    let onu_dir = top_path(&["tmp", &tonu])?;
    ensure::run(log, &["pfexec", &onu.to_str().unwrap(), "-v",
        "-d", &onu_dir.to_str().unwrap(),
        "-t", &bename])?;

    info!(log, "onu complete!  you must now reboot");
    Ok(())
}

fn cmd_illumos_genenv(ca: &CommandArg) -> Result<()> {
    let mut opts = baseopts();
    opts.optopt("g", "", "use an external gate directory", "DIR");

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

    let gate = if let Some(gate) = res.opt_str("g") {
        abs_path(gate)?
    } else {
        top_path(&["projects", "illumos"])?
    };

    regen_illumos_sh(ca.log, &gate, BuildType::Quick)?;
    regen_illumos_sh(ca.log, &gate, BuildType::QuickDebug)?;
    regen_illumos_sh(ca.log, &gate, BuildType::Full)?;
    regen_illumos_sh(ca.log, &gate, BuildType::Release)?;

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

    let gate = top_path(&["projects", "illumos"])?;
    regen_illumos_sh(ca.log, &gate, t)?;

    let env = rel_path(Some(&gate), &[t.script_name()])?;
    let src = rel_path(Some(&gate), &["usr", "src"])?;
    let bldenv = rel_path(Some(&gate), &["usr", "src",
        "tools", "scripts", "bldenv"])?;

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

fn extract_pkgs(_log: &Logger, dir: &Path) -> Result<Vec<BuildPackage>> {
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

fn cmd_build_omnios(ca: &CommandArg) -> Result<()> {
    let opts = baseopts();

    let usage = || {
        println!("{}", opts.usage("Usage: helios [OPTIONS] build-omnios \
            [OPTIONS]"));
    };

    let log = ca.log;
    let res = opts.parse(ca.args)?;

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    let dir = top_path(&["projects", "omnios-build", "build"])?;

    let mut pkgs = extract_pkgs(log, &dir)?;

    pkgs.sort_by(|a, b| a.name.cmp(&b.name));

    for pkg in pkgs.iter() {
        println!(" * {}", pkg.name);
        println!("   {:?}", pkg.file);
    }

    Ok(())
}

fn cargo_target_cmd(project: &str, command: &str, debug: bool)
    -> Result<String>
{
    let bin = top_path(&["projects", project, "target",
        if debug { "debug" } else { "release" }, command])?;
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
            bail!("proto {:?} contains a /bin directory; should use /usr/bin",
                proto);
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
            } else if relpath.starts_with("lib")
                || relpath.starts_with("usr")
            {
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

fn cmd_image(ca: &CommandArg) -> Result<()> {
    let mut opts = baseopts();
    opts.optflag("d", "", "use DEBUG packages");
    opts.optopt("g", "", "use an external gate directory", "DIR");
    opts.optopt("s", "", "tempdir name suffix", "SUFFIX");
    opts.optopt("o", "", "output directory for image", "DIR");
    opts.optmulti("F", "", "pass extra image builder features", "KEY[=VAL]");
    opts.optflag("B", "", "include omicron1 brand");
    opts.optopt("C", "", "compliance dock location", "DOCK");
    opts.optopt("N", "name", "image name", "NAME");
    opts.optflag("R", "", "recovery image");
    opts.optmulti("X", "", "skip this phase", "PHASE");
    opts.optflag("", "ddr-testing", "build ROMs for other DDR frequencies");
    opts.optopt("p", "", "use an external package repository", "PUBLISHER=URL");
    opts.optopt("P", "", "include all files from extra proto area", "DIR");

    let usage = || {
        println!("{}",
            opts.usage("Usage: helios [OPTIONS] experiment-image [OPTIONS]"));
    };

    let log = ca.log;
    let res = opts.parse(ca.args)?;
    let cdock = res.opt_str("C");
    let brand = res.opt_present("B") || cdock.is_some();
    let (publisher, extrepo) = if let Some(arg) = res.opt_str("p") {
        if let Some((key, val)) = arg.split_once('=') {
            (key.to_string(), Some(val.to_string()))
        } else {
            bail!("-p argument must be PUBLISHER=URL");
        }
    } else {
        ("on-nightly".to_string(), None)
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

    let user = illumos::get_username()?.unwrap_or("unknown".to_string());

    let image_name = res.opt_str("N").unwrap_or_else(|| {
        let now: OffsetDateTime = SystemTime::now().into();
        let dt_fmt = format_description::parse(DATE_FORMAT_STR).unwrap();
        format!("{}@{}: {}", user, illumos::nodename(),
            now.format(&dt_fmt).unwrap())
    });

    if res.opt_present("help") {
        usage();
        return Ok(());
    }

    if !res.free.is_empty() {
        bail!("unexpected arguments");
    }

    if res.opt_present("d") && extrepo.is_some() {
        /*
         * At present, our -d flag attempts to find "nightly" instead of
         * "nightly-nd" bits.  If we are using an external repository, we'll
         * have to make sure it has the DEBUG bits under a variant; an exercise
         * for later.
         */
        bail!("-d and -p are mutually exclusive");
    }

    /*
     * Check for the commands we need before we start doing any expensive work.
     */
    let builder = cargo_target_cmd("image-builder", "image-builder", true)?;
    let mkimage = cargo_target_cmd("bootserver", "mkimage", false)?;
    let pinprick = cargo_target_cmd("pinprick", "pinprick", false)?;
    let ahib = cargo_target_cmd("amd-host-image-builder",
        "amd-host-image-builder", true)?;
    let baseline = "/usr/lib/brand/omicron1/baseline";
    if brand && !PathBuf::from(baseline).is_file() {
        bail!("pkg install /system/zones/brand/omicron1/tools");
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
        bail!("ZFS dataset {:?} does not exist; we need it to create images",
            imgds);
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

    let repo = if let Some(extrepo) = &extrepo {
        /*
         * If we have been instructed to use a repository URL, we do not need to
         * do local transformation.  That transformation was done as part of
         * publishing the packages.
         */
        info!(log, "using external package repository {}", extrepo);
        None
    } else {
        /*
         * In order to install development illumos bits, we first need to elide
         * any files that would conflict with packages delivered from other
         * consolidations.  To do this, we create an onu-specific repository:
         */
        info!(log, "creating temporary repository...");
        Some(create_transformed_repo(log, &gate, &tempdir,
            res.opt_present("d"), false)?)
    };

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
            cmd.arg("-F").arg(&format!("genproto={}",
                genproto.to_str().unwrap()));
        }
        if let Some(cdock) = &cdock {
            cmd.arg("-F").arg("compliance");
            cmd.arg("-F").arg("stress");
            cmd.arg("-E").arg(&cdock);
        }
        cmd.arg("-E").arg(&brand_extras);
        cmd.arg("-E").arg(&projects_extras);
        cmd.arg("-F").arg(format!("repo_publisher={}", publisher));
        if let Some(url) = &extrepo {
            cmd.arg("-F").arg(format!("repo_url={}", url));
        } else if let Some(repo) = &repo {
            cmd.arg("-F").arg(format!("repo_redist={}",
                repo.to_str().unwrap()));
        }
        cmd.arg("-F").arg("baud=3000000");
        if brand {
            cmd.arg("-F").arg("omicron1");
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

            ensure::run(log, &[baseline,
                "-R", &root,
                &brand_extras.to_str().unwrap()
            ])?;
        }

        info!(log, "image builder template: ramdisk-02-trim...");
        let mut cmd = basecmd();
        cmd.arg("-n").arg("ramdisk-02-trim");
        ensure::run2(log, &mut cmd)?;
    } else {
        info!(log, "skipping installation phase, using existing archive");
    }

    let tname = if recovery { "zfs-recovery" }
        else if cdock.is_some() { "zfs-compliance" }
        else { "zfs" };
    info!(log, "image builder template: {}...", tname);
    let mut cmd = basecmd();
    cmd.arg("-n").arg(tname);
    ensure::run2(log, &mut cmd)?;

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
    let target_size = if cdock.is_some() {
        /*
         * In the compliance rack we would like to avoid running out of space,
         * and we have no customer workloads, so using more RAM for the ramdisk
         * pool is OK.
         */
        16 * 1024
    } else {
        4 * 1024
    };
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

    tar.add_file(&zfsimg, "zfs.img")?;

    /*
     * Create the boot archive (CPIO) with the kernel and modules that we need
     * to boot and mount the ramdisk.
     * XXX This should be an image-builder feature.
     */
    let mkcpio = top_path(&["image", "mkcpio.sh"])?;
    let cpio = rel_path(Some(&outdir), &["cpio"])?;
    info!(log, "creating boot archive (CPIO)...");
    ensure::run(log, &["bash", mkcpio.to_str().unwrap(),
        &root, cpio.to_str().unwrap(), tempdir.to_str().unwrap()])?;

    /*
     * Create a compressed CPIO and kernel to be passed to nanobl-rs via XMODEM.
     * These will also be included in the archive, just in case they are
     * required for engineering activities later.
     */
    let cpioz = rel_path(Some(&outdir), &["cpio.z"])?;
    let unix = format!("{}/platform/oxide/kernel/amd64/unix", root);
    let unixz = rel_path(Some(&outdir), &["unix.z"])?;
    info!(log, "creating compressed cpio/unix for nanobl-rs...");
    ensure::run(log, &["bash", "-c",
        &format!("'{}' '{}' >'{}'", pinprick, unix,
            unixz.to_str().unwrap())])?;
    tar.add_file(&unixz, "unix.z")?;
    ensure::run(log, &["bash", "-c",
        &format!("'{}' '{}' >'{}'", pinprick, cpio.to_str().unwrap(),
            cpioz.to_str().unwrap())])?;
    tar.add_file(&cpioz, "cpio.z")?;

    /*
     * Create the reset image for the Gimlet SPI ROM:
     */
    info!(log, "creating reset image...");
    ensure::run_in(log, &top_path(&["projects", "phbl"])?,
        &["cargo", "xtask", "build", "--release",
        "--cpioz", cpioz.to_str().unwrap()])?;
    info!(log, "building host image...");
    let rom = rel_path(Some(&outdir), &["rom"])?;
    let reset = top_path(&["projects", "phbl", "target",
        "x86_64-oxide-none-elf", "release", "phbl"])?;
    let ahibdir = top_path(&["projects", "amd-host-image-builder"])?;
    ensure::run_in(log, &ahibdir, &[
        ahib.as_str(),
        "-B", "amd-firmware/GN/1.0.0.1",
        "-B", "amd-firmware/GN/1.0.0.6",
        "--config", "etc/milan-gimlet-b.efs.json5",
        "--output-file", rom.to_str().unwrap(),
        "--reset-image", reset.to_str().unwrap(),
    ])?;
    tar.add_file(&rom, "rom")?;

    if ddr_testing {
        let inputcfg = top_path(&["projects", "amd-host-image-builder",
            "etc", "milan-gimlet-b.efs.json5"])?;

        /*
         * The configuration for amd-host-image-builder is stored in JSON5
         * format.  Read the file as a generic JSON object:
         */
        let f = std::fs::read_to_string(&inputcfg)?;
        let inputcfg: serde_json::Value = json5::from_str(&f)?;

        for limit in [1600, 1866, 2133, 2400, 2667, 2933, 3200] {
            let romname = format!("rom.ddr{limit}");
            let rom = rel_path(Some(&outdir), &[&romname])?;

            /*
             * Produce a new configuration file with the specified
             * MemBusFrequencyLimit:
             */
            let tmpcfg = rel_path(Some(&tempdir),
                &[&format!("milan-gimlet-b.ddr{}.efs.json", limit)])?;
            maybe_unlink(&tmpcfg)?;
            mk_rom_config(inputcfg.clone(), &tmpcfg, limit)?;

            /*
             * Build the frequency-specific ROM file for this frequency limit:
             */
            ensure::run_in(log, &ahibdir, &[
                ahib.as_str(),
                "-B", "amd-firmware/GN/1.0.0.1",
                "-B", "amd-firmware/GN/1.0.0.6",
                "--config", tmpcfg.to_str().unwrap(),
                "--output-file", rom.to_str().unwrap(),
                "--reset-image", reset.to_str().unwrap(),
            ])?;
            tar.add_file(&rom, &romname)?;
        }
    }

    info!(log, "finishing image archive at {tarpath:?}...");
    tar.finish()?;

    info!(log, "image complete! materials are in {:?}", outdir);
    std::fs::remove_dir_all(&tempdir).ok();
    Ok(())
}

fn mk_rom_config(mut input: serde_json::Value, output: &Path, ddr_speed: u32)
    -> Result<()>
{

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

        let ee: Entry= serde_json::from_value(e.clone())?;
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

            if h.group_id != 0x3000 || h.entry_id != 0x0004 ||
                h.instance_id != 0
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
                dword.insert("MemBusFrequencyLimit".to_string(),
                    serde_json::Value::String(format!("Ddr{}", ddr_speed)));
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

        let log = log.new(o!("project" => name.to_string()));
        info!(log, "project {name}: {project:?}");

        if exists_dir(&path)? {
            info!(log, "clone {url} exists already at {path:?}");
            if project.auto_update {
                info!(log, "fetching updates for clone ...");
                let mut child = if let Some(commit) = &project.commit {
                    Command::new("git")
                        .current_dir(&path)
                        .arg("fetch")
                        .arg("origin")
                        .arg(commit)
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

                if let Some(commit) = &project.commit {
                    info!(log, "pinning to commit {commit}...");
                    let mut child = Command::new("git")
                        .current_dir(&path)
                        .arg("checkout")
                        .arg(commit)
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

            if let Some(commit) = &project.commit {
                info!(log, "fetching commit {commit} for clone ...");
                let mut child = Command::new("git")
                    .current_dir(&path)
                    .arg("fetch")
                    .arg("origin")
                    .arg(commit)
                    .spawn()?;

                let exit = child.wait()?;
                if !exit.success() {
                    bail!("fetch in {} failed", path.display());
                }

                info!(log, "pinning to commit {commit}...");
                let mut child = Command::new("git")
                    .current_dir(&path)
                    .arg("checkout")
                    .arg(commit)
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
            site_sh += &format!("RELVER={}\n", RELVER);
            site_sh += &format!("DASHREV={}\n", DASHREV);
            site_sh += "PVER=$RELVER.$DASHREV\n";
            site_sh += "IPS_REPO=https://pkg.oxide.computer/helios-dev\n";
            site_sh += &format!("TMPDIR={}\n", &tmp.to_str().unwrap());
            site_sh += "DTMPDIR=$TMPDIR\n";

            ensure::file_str(&log, &site_sh, &ssp, 0o644,
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

    regen_publisher_mog(log, NO_PATH, publisher)?;
    for mog in &["os-conflicts", "os-deps"] {
        let mogpath = top_path(&["packages", &format!("{}.mogrify", mog)])?;
        ensure::symlink(log, &mogpath,
            &format!("../tools/packages/{}.mogrify", mog))?;
    }

    let gate = top_path(&["projects", "illumos"])?;
    regen_illumos_sh(log, &gate, BuildType::Full)?;
    regen_illumos_sh(log, &gate, BuildType::QuickDebug)?;
    regen_illumos_sh(log, &gate, BuildType::Quick)?;
    regen_illumos_sh(log, &gate, BuildType::Release)?;

    /*
     * Perform setup in project repositories that require it.
     */
    for (name, project) in p.project.iter().filter(|p| p.1.cargo_build) {
        let path = top_path(&["projects", &name])?;
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
        name: "build-omnios".into(),
        desc: "build-omnios".into(),
        func: cmd_build_omnios,
        hide: true,
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

    let usage = || {
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

        let ca = CommandArg {
            log: &log,
            args: args.as_slice(),
        };

        return (ci.func)(&ca);
    }

    bail!("command \"{}\" not understood", res.free[0]);
}
