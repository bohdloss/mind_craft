use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::Result;
use expanduser::expanduser;
use file_guard::{FileGuard, Lock};
use serde::{Deserialize, Serialize};
use serenity::all::ChannelId;
use yapper::ServerStatus;

const LOCK: &str = "~/.mcayb.lock";
pub const CONFIG: &str = "mcayb.json";

pub fn acquire_lock() -> Result<FileGuard<Box<File>>> {
    use anyhow::Context;

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(expanduser(LOCK).context("Failed to find home directory")?)
        .context("Failed to *open/create* the lock file")?;

    Ok(
        file_guard::try_lock(Box::new(lock), Lock::Exclusive, 0, isize::MAX as _)
            .context("Failed to *lock* the lock file")?,
    )
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct MCAYB {
	pub last_status: Vec<ServerStatus>,
    pub update_receivers: Vec<ChannelId>,
}
