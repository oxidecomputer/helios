use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::maybe_unlink;
use anyhow::{bail, Result};
use helios_build_utils::metadata::Metadata;

enum Act {
    File(String, PathBuf),
    Complete,
}

/**
 * Create a tar file with the gzip compressor running in another thread.  Files
 * are pushed from the main thread into a channel, where the worker thread adds
 * files to the archive as directed.  The result, success or error, is made
 * available to the user when they join the worker thread.
 */
pub struct Archive {
    tx: mpsc::Sender<Act>,
    hdl: JoinHandle<Result<()>>,
}

impl Archive {
    pub fn new(p: &Path, m: Metadata) -> Result<Archive> {
        let path = p.to_path_buf();

        maybe_unlink(&path)?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)?;
        let gzw = flate2::write::GzEncoder::new(f, flate2::Compression::best());
        let mut tar = tar::Builder::new(gzw);
        let mtime =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        /*
         * Append the metadata file first in the archive.
         */
        m.append_to_tar(&mut tar)?;

        /*
         * Append the image/ directory under which all of our data files will be
         * included:
         */
        {
            let mut h = tar::Header::new_ustar();

            h.set_entry_type(tar::EntryType::Directory);
            h.set_username("root")?;
            h.set_uid(0);
            h.set_groupname("root")?;
            h.set_gid(0);
            h.set_path("image")?;
            h.set_mtime(mtime);
            h.set_mode(0o755);
            h.set_size(0);
            h.set_cksum();

            tar.append(&h, std::io::empty())?;
        }

        let (tx, rx) = mpsc::channel();

        let hdl = std::thread::spawn(move || -> Result<()> {
            loop {
                match rx.recv().unwrap() {
                    Act::File(name, path) => {
                        let f = std::fs::OpenOptions::new()
                            .create(false)
                            .read(true)
                            .open(&path)?;

                        let mut h = tar::Header::new_ustar();

                        h.set_entry_type(tar::EntryType::Regular);
                        h.set_metadata(&f.metadata()?);
                        h.set_mode(0o444);
                        h.set_username("root")?;
                        h.set_uid(0);
                        h.set_groupname("root")?;
                        h.set_uid(0);
                        h.set_path(&name)?;
                        h.set_mtime(mtime);
                        h.set_cksum();

                        tar.append(&h, f)?;
                    }
                    Act::Complete => break,
                }
            }

            let gzw = tar.into_inner()?;
            let mut f = gzw.finish()?;
            f.flush()?;
            Ok(())
        });

        Ok(Archive { tx, hdl })
    }

    pub fn add_file(&self, p: &Path, n: &str) -> Result<()> {
        if !p.is_file() {
            bail!("{p:?} is not a file");
        }

        if n.contains("/") {
            bail!("{n:?} must be a bare file name, not directory components");
        }

        self.tx.send(Act::File(format!("image/{n}"), p.to_path_buf()))?;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        self.tx.send(Act::Complete)?;
        self.hdl.join().unwrap()
    }
}
