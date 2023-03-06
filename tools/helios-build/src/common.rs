/*
 * Copyright 2020 Oxide Computer Company
 */

#![allow(unused)]

use atty::Stream;
use slog::{Drain, Logger};
use std::sync::Mutex;
use serde::Deserialize;
use std::fs::File;
use std::io::{Read, BufReader};
use std::path::{Path, PathBuf};
use anyhow::{Result, bail};

pub use slog::{info, warn, error, debug, trace, o};

/**
 * Initialise a logger which writes to stdout, and which does the right thing on
 * both an interactive terminal and when stdout is not a tty.
 */
pub fn init_log() -> Logger {
    let dec = slog_term::TermDecorator::new().stdout().build();
    if atty::is(Stream::Stdout) {
        let dr = Mutex::new(slog_term::CompactFormat::new(dec)
            .build()).fuse();
        slog::Logger::root(dr, o!())
    } else {
        let dr = Mutex::new(slog_term::FullFormat::new(dec)
            .use_original_order()
            .build()).fuse();
        slog::Logger::root(dr, o!())
    }
}

pub fn sleep(s: u64) {
    std::thread::sleep(std::time::Duration::from_secs(s));
}

pub trait OutputExt {
    fn info(&self) -> String;
}

impl OutputExt for std::process::Output {
    fn info(&self) -> String {
        let mut out = String::new();

        if let Some(code) = self.status.code() {
            out.push_str(&format!("exit code {}", code));
        }

        /*
         * Attempt to render stderr from the command:
         */
        let stderr = String::from_utf8_lossy(&self.stderr).trim().to_string();
        let extra = if stderr.is_empty() {
            /*
             * If there is no stderr output, this command might emit its
             * failure message on stdout:
             */
            String::from_utf8_lossy(&self.stdout).trim().to_string()
        } else {
            stderr
        };

        if !extra.is_empty() {
            if !out.is_empty() {
                out.push_str(": ");
            }
            out.push_str(&extra);
        }

        out
    }
}

pub fn read_toml<P, O>(path: P) -> Result<O>
    where P: AsRef<Path>,
          for<'de> O: Deserialize<'de>
{
    let f = File::open(path.as_ref())?;
    let mut buf: Vec<u8> = Vec::new();
    let mut r = BufReader::new(f);
    r.read_to_end(&mut buf)?;
    Ok(toml::from_slice(&buf)?)
}

fn exists<P: AsRef<Path>>(path: P) -> Result<Option<std::fs::Metadata>> {
    let p = path.as_ref();
    match std::fs::metadata(p) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => bail!("checking for path {}: {}", p.display(), e),
    }
}

pub fn exists_file<P: AsRef<Path>>(path: P) -> Result<bool> {
    let p = path.as_ref();

    if let Some(m) = exists(p)? {
        if m.is_file() {
            Ok(true)
        } else {
            bail!("path {} exists but is not a file", p.display());
        }
    } else {
        Ok(false)
    }
}

pub fn exists_dir<P: AsRef<Path>>(path: P) -> Result<bool> {
    let p = path.as_ref();

    if let Some(m) = exists(p)? {
        if m.is_dir() {
            Ok(true)
        } else {
            bail!("path {} exists but is not a directory", p.display());
        }
    } else {
        Ok(false)
    }
}

pub fn unprefix(prefix: &Path, path: &Path) -> Result<PathBuf> {
    if prefix.is_absolute() != path.is_absolute() {
        bail!("prefix and path must not be a mix of absolute and relative");
    }

    let cprefix = prefix.components().collect::<Vec<_>>();
    let cpath = path.components().collect::<Vec<_>>();

    if let Some(tail) = cpath.strip_prefix(cprefix.as_slice()) {
        Ok(tail.iter().collect())
    } else {
        bail!("{:?} does not start with prefix {:?}", path, prefix);
    }
}

pub fn reprefix(prefix: &Path, path: &Path, target: &Path) -> Result<PathBuf> {
    if !target.is_absolute() {
        bail!("target must be absolute");
    }
    let mut newpath = target.to_path_buf();
    newpath.push(unprefix(prefix, path)?);
    Ok(newpath)
}
