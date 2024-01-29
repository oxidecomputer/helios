/*
 * Copyright 2024 Oxide Computer Company
 */

use anyhow::{bail, Result};
use serde::Deserialize;
use slog::{Drain, Logger};
use std::io::IsTerminal;
use std::path::Path;
use std::sync::Mutex;

pub use slog::{info, o};

/**
 * Initialise a logger which writes to stdout, and which does the right thing on
 * both an interactive terminal and when stdout is not a tty.
 */
pub fn init_log() -> Logger {
    let dec = slog_term::TermDecorator::new().stdout().build();
    if std::io::stdout().is_terminal() {
        let dr = Mutex::new(slog_term::CompactFormat::new(dec).build()).fuse();
        slog::Logger::root(dr, o!())
    } else {
        let dr = Mutex::new(
            slog_term::FullFormat::new(dec).use_original_order().build(),
        )
        .fuse();
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
where
    P: AsRef<Path>,
    for<'de> O: Deserialize<'de>,
{
    Ok(toml::from_str(&std::fs::read_to_string(path.as_ref())?)?)
}

fn exists<P: AsRef<Path>>(path: P) -> Result<Option<std::fs::Metadata>> {
    let p = path.as_ref();
    match std::fs::metadata(p) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => bail!("checking for path {}: {}", p.display(), e),
    }
}

#[allow(unused)]
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

/**
 * Try to unlink a file.  If it did not exist, treat that as a success; report
 * any other error.
 */
pub fn maybe_unlink(f: &Path) -> Result<()> {
    match std::fs::remove_file(f) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => bail!("could not remove {f:?}: {e:?}"),
    }
}
