use std::process::Command;

use anyhow::{Result, bail};

pub fn dataset_exists(dataset: &str) -> Result<bool> {
    if dataset.contains('@') {
        bail!("no @ allowed here");
    }

    let zfs = Command::new("/sbin/zfs")
        .env_clear()
        .arg("list")
        .arg("-Ho").arg("name")
        .arg(dataset)
        .output()?;

    if !zfs.status.success() {
        let errmsg = String::from_utf8_lossy(&zfs.stderr);
        if errmsg.trim().ends_with("dataset does not exist") {
            return Ok(false);
        }
        bail!("zfs list failed: {}", errmsg);
    }

    Ok(true)
}

pub fn zfs_get(dataset: &str, n: &str) -> Result<String> {
    let zfs = Command::new("/sbin/zfs")
        .env_clear()
        .arg("get")
        .arg("-H")
        .arg("-o").arg("value")
        .arg(n)
        .arg(dataset)
        .output()?;

    if !zfs.status.success() {
        let errmsg = String::from_utf8_lossy(&zfs.stderr);
        bail!("zfs get failed: {}", errmsg);
    }

    let out = String::from_utf8(zfs.stdout)?;
    Ok(out.trim().to_string())
}
