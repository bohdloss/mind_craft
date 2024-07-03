use std::fs::File;
use std::sync::{Arc, RwLock};
use file_guard::FileGuard;
use yapper::conf::Config;
use crate::config::SVManage;
use crate::server_loop::Server;

pub struct Ctxt {
	#[allow(dead_code)]
	pub lock: FileGuard<Box<File>>,
	pub config: Config<SVManage>,
	pub servers: Vec<Server>,
}