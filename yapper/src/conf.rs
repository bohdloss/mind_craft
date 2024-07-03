use std::fs::{File, OpenOptions};
use std::io;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, RwLock};
use serde::{Deserialize, Serialize};
use anyhow::Result;

#[derive(Debug)]
struct ConfigInner<T: Sync + Send + Default + Serialize + for<'a> Deserialize<'a>> {
	config: T,
	file: File,
}

#[derive(Debug, Clone)]
pub struct Config<T: Sync + Send + Default + Serialize + for<'a> Deserialize<'a>>(Arc<RwLock<ConfigInner<T>>>);

impl<T: Sync + Send + Default + Serialize + for<'a> Deserialize<'a>> Config<T> {
	pub fn init(path: &str) -> Result<Self> {
		use anyhow::Context;

		if !Path::new(path).is_file() {
			let mut config_file = File::create(path)?;
			let default = T::default();
			serde_json::to_writer_pretty(&mut config_file, &default)
				.context("Failed to write the default config")?;
			Ok(Self(Arc::new(RwLock::new(ConfigInner {
				config: default,
				file: config_file,
			}))))
		} else {
			let mut config_file = OpenOptions::new()
				.write(true)
				.read(true)
				.create(true)
				.open(path)
				.context("Failed to open the config file")?;

			let config: T =
				serde_json::from_reader(&mut config_file).context("Failed to parse config file")?;

			Ok(Self(Arc::new(RwLock::new(ConfigInner {
				config,
				file: config_file,
			}))))
		}
	}

	pub fn with_config<F, R>(&self, f: F) -> R
	where
		F: FnOnce(&T) -> R,
	{
		let this = self.0.read().unwrap();
		f(&this.config)
	}

	pub fn with_config_mut<F, R>(&self, f: F) -> Result<R>
	where
		F: FnOnce(&mut T) -> R,
	{
		use anyhow::Context;

		let mut this = self.0.write().unwrap();
		let r = f(&mut this.config);
		let flush: io::Result<()> = try {
			this.file.set_len(0)?;
			this.file.seek(SeekFrom::Start(0))?;
			let mut vec: Vec<u8> = Vec::new();
			serde_json::to_writer_pretty(&mut vec, &this.config)?;
			this.file.write_all(&vec)?;
			this.file.flush()?;
		};
		flush.context("Failed to flush configuration changes")?;

		Ok(r)
	}
}