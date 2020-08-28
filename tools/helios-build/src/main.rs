mod common;
use common::*;

pub mod illumos;
pub mod ensure;

use anyhow::{Result, bail};
use serde::{Serialize, Deserialize};
use std::collections::{BTreeMap, HashMap};
use std::process::Command;
use std::io::{BufWriter, BufReader, Write};
use slog::Logger;
use illumos::ZonesExt;

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

fn ensure_dir(components: &[&str]) -> Result<()> {
    let dir = top_path(components)?;
    if !exists_dir(&dir)? {
        std::fs::create_dir(&dir)?;
    }
    Ok(())
}


#[derive(Debug, Deserialize)]
struct UserlandMetadata {
    dependencies: Vec<String>,
    fmris: Vec<String>,
    name: String,
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
     * Make sure the metadata is up-to-date for this component.
     */
    let targetdir = top_path(&["projects", "userland", "components",
        &target])?;
    ensure::run_utf8(log, &["/usr/bin/gmake", "-s",
        "-C", targetdir.to_str().unwrap(),
        "update-metadata"])?;
    let mut mdf = targetdir.clone();
    mdf.push("pkg5");
    let f = std::fs::File::open(&mdf)?;
    let umd: UserlandMetadata = serde_json::from_reader(&f)?;

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
     */
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
        for i in install.iter() {
            args.push(i);
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
        cd '{}'\n\
        /usr/bin/gmake publish\n",
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
        serde_json::from_reader(std::fs::File::open(&datafile)?)?;

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
        if let Ok(f) = std::fs::File::open(&datafile) {
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

    for (name, project) in p.project.iter() {
        let path = top_path(&["projects", &name])?;
        let url = project.url(false)?;

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
    }

    /*
     * Create the package repository that will contain the final output
     * packages after build and transformations are applied.
     */
    let publisher = "helios-dev";
    ensure_dir(&["packages"])?;
    let repo_path = top_path(&["packages", "repo"])?;
    if !exists_dir(&repo_path)? {
        let path = repo_path.to_str().unwrap(); /* XXX */

        /*
         * XXX make this more idempotent...
         */
        ensure::run(log, &["/usr/bin/pkgrepo", "create", &path])?;
        ensure::run(log, &["/usr/bin/pkgrepo", "add-publisher", "-s",
            &path, &publisher])?;
    }

    /*
     * Create the pkgmogrify template that we need to replace the pkg(5)
     * publisher name when promoting packages from a build repository to the
     * central repository.
     */
    let mog = format!("<transform set name=pkg.fmri -> \
        edit value pkg://[^/]+/ pkg://{}/>", publisher);
    let mogpath = top_path(&["packages", "publisher.mogrify"])?;
    ensure::file_str(log, &mog, &mogpath, 0o644, ensure::Create::Always)?;

    /*
     * Perform setup in userland repository.
     */
    let userland_path = top_path(&["projects", "userland"])?;
    if exists_dir(&userland_path)? {
        let p = userland_path.to_str().unwrap(); /* XXX */

        ensure::run(log, &["/usr/bin/gmake", "-C", &p, "setup"])?;
    }

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
        desc: "setup".into(),
        func: cmd_setup,
        hide: false,
    });
    handlers.push(CommandInfo {
        name: "zone".into(),
        desc: "zone".into(),
        func: cmd_zone,
        hide: false,
    });
    handlers.push(CommandInfo {
        name: "build".into(),
        desc: "build".into(),
        func: cmd_build,
        hide: false,
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
