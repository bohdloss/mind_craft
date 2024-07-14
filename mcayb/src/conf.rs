use anyhow::Result;
use expanduser::expanduser;
use file_guard::{FileGuard, Lock};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serenity::all::{ChannelId, GuildId, MessageId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};
use std::fmt::Formatter;
use std::str::FromStr;
use ende::{Decode, Encode};
use semver::Version;
use serde::de::{Error, MapAccess, Visitor};
use yapper::{base64_decode, base64_encode, ServerStatus};

const LOCK: &str = "~/.mcayb.lock";
pub const CONFIG: &str = "mcayb.json";

pub const VERSION: Version = Version::new(1, 2, 0);

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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MCAYB {
    pub version: Version,
    pub guild_data: HashMap<GuildId, GuildData>,
}

impl Default for MCAYB {
    fn default() -> Self {
        Self {
            version: VERSION,
            guild_data: HashMap::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct GuildData {
    pub sv_user: String,
    pub sv_pass: [u8; 32],
    pub last_status: Vec<ServerStatus>,
    pub notifications: Option<ChannelId>,
    pub mod_polls: HashMap<ModKey, ModPoll>,
}

#[derive(Debug, Encode, Decode, Hash, Clone, Eq, PartialEq)]
pub struct ModKey {
    pub server: String,
    pub mod_id: String,
}

#[derive(Serialize, Deserialize)]
struct StringHolder {
    pub a: String,
    pub b: String,
}

impl Serialize for ModKey {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let this = ende::encode_bytes(self).unwrap();
        let this = base64_encode(&this);
        this.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ModKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let this = String::deserialize(deserializer)?;
        let this = base64_decode(&this);
        let this = ende::decode_bytes(&this).unwrap();
        Ok(this)
    }
}

impl ModKey {
    pub fn new(server: impl Into<String>, mod_id: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            mod_id: mod_id.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum PollKind {
    Install,
    Remove
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModPoll {
    pub channel: ChannelId,
    pub poll: MessageId,
    pub file: PathBuf,
    pub preferred_name: String,
    pub kind: PollKind
}
