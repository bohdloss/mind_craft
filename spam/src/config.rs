use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::ops::{Index, IndexMut};
use std::time::Instant;
use bitflags::bitflags;
use chrono::{DateTime, Utc};
use expanduser::expanduser;
use file_guard::{FileGuard, Lock};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CONFIG: &str = "spam.json";
const LOCK: &str = "~/.spam.lock";

pub fn acquire_lock() -> anyhow::Result<FileGuard<Box<File>>> {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SPAM {
	pub api_tokens: HashMap<Uuid, Token>
}

bitflags! {
	#[derive(Debug, Clone, Serialize, Deserialize)]
	pub struct Access: u8 {
		const Read = 0b00000001;
		const Write = 0b00000010;
	}
}

impl Access {
	pub const NONE: Self = Access::empty();
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
#[allow(non_camel_case_types)]
pub enum Scope {
	Assets_Mods,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
	created: DateTime<Utc>,
	expires: Option<DateTime<Utc>>,
	scopes: HashMap<Scope, Access>,
}

impl Token {
	pub fn new() -> Self {
		let mut scopes = HashMap::new();
		scopes.insert(Scope::Assets_Mods, Access::NONE);
		
		Self {
			created: Utc::now(),
			expires: None,
			scopes
		}
	}
}

impl Index<Scope> for Token {
	type Output = Access;
	fn index(&self, index: Scope) -> &Self::Output {
		&self.scopes[&index]
	}
}

impl IndexMut<Scope> for Token {
	fn index_mut(&mut self, index: Scope) -> &mut Self::Output {
		self.scopes.get_mut(&index).unwrap()
	}
}