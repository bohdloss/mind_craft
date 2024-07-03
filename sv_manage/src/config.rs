use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use yapper::{hash_pw, Notification, ServerStatus};

pub const CONFIG: &str = "sv_manage.json";

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SVManage {
	pub cache: Cache,
	pub gateway: GatewayConf,
	pub servers: HashMap<String, ServerConf>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Cache {
	notifications: Vec<Notification>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GatewayConf {
	pub port: u16,
	pub pw_sha256: [u8; 32],
}

impl Default for GatewayConf {
	fn default() -> Self {
		Self {
			port: 23786,
			pw_sha256: hash_pw("12345678"),
		}
	}
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
