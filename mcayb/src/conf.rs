use std::{
    fs::{File, OpenOptions},
};
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use ende::{Decode, Encode};
use expanduser::expanduser;
use file_guard::{FileGuard, Lock};
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serenity::all::{ChannelId, GuildId, MessageId};

use yapper::{base64_decode, base64_encode, ModInfo, ServerStatus};

const LOCK: &str = "~/.mcayb.lock";
pub const CONFIG: &str = "mcayb.json";

pub const VERSION: Version = Version::new(1, 3, 0);

macro_rules! string_serde {
    ($item:ident) => {
        impl Serialize for $item {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let this = ende::encode_bytes(self).unwrap();
                let this = base64_encode(&this);
                this.serialize(serializer)
            }
        }
        
        impl<'de> Deserialize<'de> for $item {
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
    };
}

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
    pub notifications: ChannelId,
    pub polls: HashMap<PollKey, OngoingPoll>,
}

#[derive(Debug, Encode, Decode, Hash, Clone, Eq, PartialEq)]
pub enum PollKey {
    Mod {
        server: String,
        mod_id: String,
    },
    Restore {
        server: String
    }
}

string_serde!(PollKey);

impl PollKey {
    pub fn mod_op(server: impl Into<String>, mod_id: impl Into<String>) -> Self {
        Self::Mod {
            server: server.into(),
            mod_id: mod_id.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum PollKind {
    Install {
        server: String,
        info: ModInfo,
    },
    Remove {
        server: String,
        info: ModInfo
    },
    Restore {
        server: String
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OngoingPoll {
    pub channel: ChannelId,
    pub poll: MessageId,
    pub kind: PollKind
}

#[derive(Debug, Encode, Decode, Clone, Eq, PartialEq)]
pub enum TransactionKey {
    Mod {
        server: String
    },
    Other
}

string_serde!(TransactionKey);

impl TransactionKey {
    pub fn mod_op(server: impl Into<String>) -> Self {
        Self::Mod {
            server: server.into()
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum TransactionKind {
    InstallMod {
        the_file: PathBuf,
        additional: Vec<PathBuf>
    },
    RemoveMod {
        mod_id: String
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub kind: TransactionKind
}