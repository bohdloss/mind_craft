use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use yapper::{hash_pw, Notification, ServerStatus};

pub const CONFIG: &str = "sv_manage.json";

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SVManage {
	pub port: u16,
	pub accounts: HashMap<String, AccountData>
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct AccountData {
	pub cache: Cache,
	pub password: [u8; 32],
	pub servers: HashMap<String, ServerConf>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Cache {
	notifications: Vec<Notification>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServerConf {
	pub running: bool,
	pub path: String,
}

impl Default for ServerConf {
	fn default() -> Self {
		Self {
			running: false,
			path: Default::default()
		}
	}
}
