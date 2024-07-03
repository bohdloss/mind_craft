use std::cell::Cell;
use std::fs;
use std::path::Path;
use anyhow::{anyhow, bail, Context, Result};

pub struct Progress {
	pub copied: u64,
	pub total: u64,
}

pub fn copy_dir_all<P, Q, F>(from: P, to: Q, mut progress: F) -> Result<()>
where P: AsRef<Path>,
      Q: AsRef<Path>,
      F: FnMut(Progress)
{
	let total = fs_extra::dir::get_size(from.as_ref()).context("Failed to get total folder size")?;
	let mut copied = 0;
	copy_dir_all_recursive(from, to, &mut progress, total, &mut copied)
}

fn copy_dir_all_recursive<P, Q, F>(from: P, to: Q, progress: &mut F, total: u64, copied: &mut u64) -> Result<()>
where P: AsRef<Path>,
      Q: AsRef<Path>,
      F: FnMut(Progress)
{
	if !from.as_ref().is_dir() {
		bail!("Source directory doesn't exist!")
	}
	if to.as_ref().is_file() {
		bail!("Destination directory is actually a file??")
	}
	if !to.as_ref().is_dir() {
		fs::create_dir_all(to.as_ref()).context("Failed to create destination directory")?;
	}
	for entry in fs::read_dir(from.as_ref()).context("Failed to open directory")? {
		let entry = entry.context("Directory iteration error")?;
		match entry.file_type().context("Failed to get file type")? {
			file_type if file_type.is_file() => {
				let size = entry.metadata()
					.context("Failed to get file metadata")?
					.len();

				let from_file = entry.path();
				let to_file = to.as_ref().join(entry.file_name());
				
				fs::copy(&from_file, &to_file).with_context(|| anyhow!("Error copying file: From({from_file:?}), To({to_file:?})"))?;
				*copied += size;
				
				progress(Progress {
					copied: *copied,
					total
				})
			}
			file_type if file_type.is_symlink() => {
				bail!("Symlink and idk what to do!")
			}
			file_type if file_type.is_dir() => {
				let from_dir = entry.path();
				let to_dir = to.as_ref().join(entry.file_name());
				
				copy_dir_all_recursive(&from_dir, &to_dir, progress, total, copied)
					.with_context(|| anyhow!("Copying sub-directory failed: From({from_dir:?}), To({to_dir:?})"))?;
			}
			unknown_type => bail!("Unsupported file type: {unknown_type:?}")
		}
	}

	Ok(())
}