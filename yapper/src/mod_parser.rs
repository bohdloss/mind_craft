use std::collections::HashMap;
use std::io::Read;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;
use anyhow::{anyhow, bail, Result};
use derive_ex::derive_ex;
use ende::{Decode, Encode, Encoder, EncodingResult, val_error};
use ende::io::{SeekFrom};
use mvn_version::ComparableVersion;
use regex::Regex;
use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use crate::{base64_decode, base64_encode};

pub fn reserved_mod_id(string: impl AsRef<str>) -> bool {
	let string = string.as_ref();
	string == "minecraft" || string == "forge"
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct WrappedComparableVersion(ComparableVersion);

impl<W: ende::io::Write> Encode<W> for WrappedComparableVersion {
	fn encode(&self, encoder: &mut Encoder<W>) -> EncodingResult<()> {
		self.0.to_string().encode(encoder)
	}
}

impl<R: ende::io::Read> Decode<R> for WrappedComparableVersion {
	fn decode(decoder: &mut Encoder<R>) -> EncodingResult<Self> {
		let string = String::decode(decoder)?;
		let value = ComparableVersion::new(&string);
		Ok(Self(value))
	}
}

impl From<ComparableVersion> for WrappedComparableVersion {
	fn from(value: ComparableVersion) -> Self {
		Self(value)
	}
}

impl Into<ComparableVersion> for WrappedComparableVersion {
	fn into(self) -> ComparableVersion {
		self.0
	}
}

#[derive(Clone, Eq, PartialEq, Encode, Decode)]
#[derive_ex(Debug)]
pub struct ModInfo {
	pub filename: String,
	pub path: PathBuf,
	pub mod_id: String,
	pub name: Option<String>,
	pub description: Option<String>,
	#[ende(into: WrappedComparableVersion)]
	pub version: ComparableVersion,
	#[debug(ignore)]
	pub logo: Option<Vec<u8>>,
	pub url: Option<String>,
	pub credits: Option<String>,
	pub authors: Option<Vec<String>>,
	pub dependencies: Vec<ModDependency>,
}

impl ModInfo {
	pub fn name(&self) -> &str {
		self.name.as_ref().unwrap_or(&self.mod_id)
	}
}

impl Serialize for ModInfo {
	fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let encoded = ende::encode_bytes(self).unwrap();
		let base64 = base64_encode(&encoded);
		base64.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for ModInfo {
	fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let base64 = String::deserialize(deserializer)?;
		let encoded = base64_decode(&base64);
		Ok(ende::decode_bytes(&encoded).unwrap())
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ModDependency {
	pub mod_id: String,
	pub mandatory: bool,
	pub min_version: Bound<ComparableVersion>,
	pub max_version: Bound<ComparableVersion>,
	pub side: Side
}

impl<W: ende::io::Write> Encode<W> for ModDependency {
	fn encode(&self, encoder: &mut Encoder<W>) -> EncodingResult<()> {
		self.mod_id.encode(encoder)?;
		self.mandatory.encode(encoder)?;
		let min_version: Bound<WrappedComparableVersion> = match &self.min_version {
			Bound::Included(x) => Bound::Included(x.clone().into()),
			Bound::Excluded(x) => Bound::Excluded(x.clone().into()),
			Bound::Unbounded => Bound::Unbounded,
		};
		let max_version: Bound<WrappedComparableVersion> = match &self.max_version {
			Bound::Included(x) => Bound::Included(x.clone().into()),
			Bound::Excluded(x) => Bound::Excluded(x.clone().into()),
			Bound::Unbounded => Bound::Unbounded,
		};
		min_version.encode(encoder)?;
		max_version.encode(encoder)?;
		self.side.encode(encoder)?;
		Ok(())
	}
}

impl<R: ende::io::Read> Decode<R> for ModDependency {
	fn decode(decoder: &mut Encoder<R>) -> EncodingResult<Self> {
		let mod_id = String::decode(decoder)?;
		let mandatory = bool::decode(decoder)?;
		let min_version = Bound::<WrappedComparableVersion>::decode(decoder)?;
		let max_version = Bound::<WrappedComparableVersion>::decode(decoder)?;
		let min_version: Bound<ComparableVersion> = match min_version {
			Bound::Included(x) => Bound::Included(x.into()),
			Bound::Excluded(x) => Bound::Excluded(x.into()),
			Bound::Unbounded => Bound::Unbounded,
		};
		let max_version: Bound<ComparableVersion> = match max_version {
			Bound::Included(x) => Bound::Included(x.into()),
			Bound::Excluded(x) => Bound::Excluded(x.into()),
			Bound::Unbounded => Bound::Unbounded,
		};
		let side = Side::decode(decoder)?;
		Ok(Self {
			mod_id,
			mandatory,
			min_version,
			max_version,
			side,
		})
	}
}

pub fn parse_mod(path: &Path) -> anyhow::Result<ModInfo> {
	parse_mod_ext(path, None)
}

pub fn parse_mod_ext(path: &Path, forge_ver: Option<String>) -> anyhow::Result<ModInfo> {
	use anyhow::Context;
	let file = std::fs::File::open(path).context("Failed to reopen the newly downloaded mod")?;
	let mut archive = zip::ZipArchive::new(file).context("Failed to parse zip file")?;

	let mut mod_info = archive
		.by_name("META-INF/mods.toml")
		.context("Couldn't find mod metadata")?;
	let mut mods_toml = String::new();
	mod_info
		.read_to_string(&mut mods_toml)
		.context("Failed to read mod metadata")?;
	drop(mod_info);

	let mut data: anyhow::Result<ModsToml<0>> = toml::from_str(&mods_toml).context("Failed to parse mods.toml");
	let mut data = match data {
		Ok(data) => data,
		Err(err) => {
			let data: ModsToml<1> = toml::from_str(&mods_toml).context("Failed to parse mods.toml (again)")?;
			let mut convert = Vec::new();
			for x in data.mods {
				let Mods::WithCreditList(ModsWithCreditsList {
					mod_id,
					version,
					display_name,
					logo_file,
					description,
					display_url,
					credits,
					authors,
				}) = x else { unreachable!() };

				let credits = if let Some(credits) = credits {
					let mut string = String::new();
					let mut first = true;
					for x in credits {
						if first {
							first = false;
							string.push_str(&x);
						} else {
							string.push_str(&format!(", {x}"));
						}
					}
					Some(string)
				} else { None };

				convert.push(Mods::Default(ModsDefault {
					mod_id,
					version,
					display_name,
					logo_file,
					description,
					display_url,
					credits,
					authors,
				}))
			}
			ModsToml { logo_file: data.logo_file, mods: convert, dependencies: data.dependencies }
		}
	};

	if data.mods.len() != 1 {
		bail!(
            "Expected to find metadata for 1 mod but found metadata for {}",
            data.mods.len()
        );
	}

	let mods = data.mods.remove(0);

	let logo_file = mods.logo_file().or(data.logo_file);
	let logo_data = if let Some(logo) = logo_file {
		let x: Result<Vec<u8>> = try {
			let mut logo_file = archive.by_name(&logo);
			if logo_file.is_err() {
				drop(logo_file);

				logo_file = archive.by_name(&format!("META-INF/{logo}"))
			}
			let mut logo_file = logo_file?;

			let mut logo_data = Vec::new();

			// 10 MB limit
			// let mut logo_file = Std::new(SizeLimit::new(Std::new(logo_file), 0, 1024 * 1024 * 10));

			logo_file
				.read_to_end(&mut logo_data)
				.context(format!("Failed to read logo data: {logo}"))?;
			logo_data
		};
		x.ok()
	} else { None };

	// let img = image::io::Reader::new(Cursor::new(&logo_data)).decode().context("Failed to load logo image")?;
	// let mut png = Vec::new();
	// img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).context("Failed to convert image to png")?;

	let version = {
		let x: Result<String> = try {
			let mut the_version = mods.version().to_owned();
			if the_version.trim() == "${file.jarVersion}" {
				let mut manifest = archive
					.by_name("META-INF/MANIFEST.MF")
					.context("Couldn't open jar manifest")?;
				let mut manifest_data = String::new();
				manifest
					.read_to_string(&mut manifest_data)
					.context("Failed to read manifest data")?;
				for line in manifest_data.split("\n") {
					if line.starts_with("Implementation-Version: ") {
						if let Some(version) = line.trim().split(" ").nth(1) {
							the_version = version.to_owned();
							break;
						}
					}
				}
			} else if the_version.trim() == "${global.forgeVersion}" {
				the_version = forge_ver.ok_or(anyhow!("Forge version needed but not provided"))?;
			}
			the_version
		};
		x.context("Failed to get version")?
	};
	let version = {
		ComparableVersion::new(version.trim())
		// // If the version contains a dash, then it's probably formatted as MC_VERSION-ACTUAL_VERSION
		// let slice = if let Some((_, second)) = version.split_once("-") {
		// 	second
		// } else {
		// 	&version
		// };
		//
		// fn is_number(ch: char) -> bool {
		// 	match ch {
		// 		'0' | '1' | '2' | '3' | '4' | '5' | '6' | '7' | '8' | '9' => true,
		// 		_ => false,
		// 	}
		// }
		//
		// let pieces: Vec<&str> = slice.split(".").collect();
		// let mut major = None;
		// let mut minor = None;
		// let mut patch = None;
		// let mut build = None;
		// // let mut pre_release = None;
		// // let mut build_meta = None;
		// for i in 0..pieces.len() {
		// 	let validate_piece = |piece: &str| -> Result<()> {
		// 		if !piece.chars().all(is_number) {
		// 			bail!("Version piece contained something that wasn't a number: (piece){piece}")
		// 		}
		// 		Ok(())
		// 	};
		//
		// 	if i == pieces.len() - 1 /* is_last */ {
		// 		let last_piece = pieces[i];
		//
		// 		// May contain additional stuff like symbols
		//
		// 		// Special case letters (1.2.c => 1.2.3)
		// 		if let mut chars = last_piece.chars() &&
		// 			let Some(ch) = chars.next() &&
		// 			let None = chars.next() &&
		// 			ch >= 'a' && ch <= 'z' {
		// 			let piece = (ch as u8 - 'a' as u8).to_string();
		// 			let piece = String::leak(piece) as &str;
		//
		// 			match i {
		// 				0 => { major = Some(piece); }
		// 				1 => { minor = Some(piece); }
		// 				2 => { patch = Some(piece); }
		// 				3 => { build = Some(piece); }
		// 				_ => bail!("Malformed version string (too long!)")
		// 			}
		// 		}
		//
		// 	} else {
		// 		let piece = pieces[i];
		// 		validate_piece(piece)?;
		//
		// 		match i {
		// 			0 => { major = Some(piece); }
		// 			1 => { minor = Some(piece); }
		// 			2 => { patch = Some(piece); }
		// 			_ => bail!("Malformed version string (too long!)")
		// 		}
		// 	}
		// }
		//
		// Version {
		// 	major: major.and_then(|x| FromStr::from_str(x).ok()).unwrap_or_default(),
		// 	minor: minor.and_then(|x| FromStr::from_str(x).ok()).unwrap_or_default(),
		// 	patch: patch.and_then(|x| FromStr::from_str(x).ok()).unwrap_or_default(),
		// 	pre: Default::default(),
		// 	build: Default::default(),
		// }
	};

	let filename: Result<String> = try {
		path
			.file_name()
			.ok_or(anyhow!("Couldn't get file name"))?
			.to_str()
			.ok_or(anyhow!("Couldn't convert to string"))?
			.to_string()
	};
	let filename = filename.unwrap_or(format!("{}.jar", mods.mod_id()));

	let authors = mods.authors().map(|authors| {
		authors
			.split(",")
			.map(|string| string.trim())
			.map(|string| string.to_owned())
			.collect()
	});

	let mut dependencies: Vec<ModDependency> = Vec::new();

	for (_, dep) in data.dependencies {
		for dep in dep {
			if dep.mod_id == mods.mod_id() { continue }
			if dependencies.iter().any(|x| x.mod_id == dep.mod_id) {
				bail!("Dependency declared twice!")
			}

			// Parse version requirement
			let (lo_bound, hi_bound) = if let Some(range) = &dep.version_range && !range.is_empty() {
				if let Some((lo, hi)) = range.split_once(",") {
					let lo = if lo.starts_with("[") {
						if lo == "[" {
							Bound::Unbounded
						} else {
							Bound::Included(ComparableVersion::new(&lo[1..]))
						}
					} else if lo.starts_with("(") {
						if lo == "[" {
							Bound::Unbounded
						} else {
							Bound::Excluded(ComparableVersion::new(&lo[1..]))
						}
					} else {
						bail!("Unparseable version range")
					};

					let hi = if hi.ends_with("]") {
						if hi == "]" {
							Bound::Unbounded
						} else {
							Bound::Included(ComparableVersion::new(&hi[..hi.len() - 1]))
						}
					} else if hi.ends_with(")") {
						if hi == ")" {
							Bound::Unbounded
						} else {
							Bound::Excluded(ComparableVersion::new(&hi[..hi.len() - 1]))
						}
					} else {
						bail!("Unparseable version range")
					};

					(lo, hi)
				} else {
					if range.starts_with("[") && range.ends_with("]") {
						let ver = ComparableVersion::new(&range[1..range.len() - 1]);
						(Bound::Included(ver.clone()), Bound::Included(ver))
					} else {
						bail!("Unparseable version range")
					}
				}
			} else {
				(Bound::Unbounded, Bound::Unbounded)
			};

			dependencies.push(ModDependency {
				mod_id: dep.mod_id.clone(),
				mandatory: dep.mandatory,
				min_version: lo_bound,
				max_version: hi_bound,
				side: dep.side,
			});
		}
	}

	Ok(ModInfo {
		filename,
		path: path.canonicalize().context("Couldn't canonicalize path")?,
		mod_id: mods.mod_id(),
		name: mods.display_name(),
		description: mods.description(),
		version,
		logo: logo_data,
		url: mods.display_url(),
		credits: mods.credits(),
		authors,
		dependencies
	})
}

#[derive(Debug, Serialize, Deserialize)]
struct ModsToml<const VARIANT: usize> {
	#[serde(rename = "logoFile")]
	logo_file: Option<String>,
	mods: Vec<Mods<VARIANT>>,
	#[serde(default)]
	dependencies: HashMap<String, Vec<Dependencies>>
}

#[derive(Debug)]
enum Mods<const VARIANT: usize> {
	Default(ModsDefault),
	WithCreditList(ModsWithCreditsList),
}

impl<const VARIANT: usize> Mods<VARIANT> {
	pub fn mod_id(&self) -> String {
		match self {
			Mods::Default(x) => x.mod_id.clone(),
			Mods::WithCreditList(x) => x.mod_id.clone(),
		}
	}

	pub fn version(&self) -> String {
		match self {
			Mods::Default(x) => x.version.clone(),
			Mods::WithCreditList(x) => x.version.clone(),
		}
	}

	pub fn display_name(&self) -> Option<String> {
		match self {
			Mods::Default(x) => x.display_name.clone(),
			Mods::WithCreditList(x) => x.display_name.clone(),
		}
	}

	pub fn logo_file(&self) -> Option<String> {
		match self {
			Mods::Default(x) => x.logo_file.clone(),
			Mods::WithCreditList(x) => x.logo_file.clone(),
		}
	}

	pub fn description(&self) -> Option<String> {
		match self {
			Mods::Default(x) => x.description.clone(),
			Mods::WithCreditList(x) => x.description.clone(),
		}
	}

	pub fn display_url(&self) -> Option<String> {
		match self {
			Mods::Default(x) => x.display_url.clone(),
			Mods::WithCreditList(x) => x.display_url.clone(),
		}
	}

	pub fn credits(&self) -> Option<String> {
		match self {
			Mods::Default(x) => x.credits.clone(),
			Mods::WithCreditList(x) => {
				let mut result = String::new();
				let mut first = true;
				for x in x.credits.as_ref()?.iter() {
					if first {
						first = false;
						result.push_str(x);
					} else {
						result.push_str(", ");
						result.push_str(x);
					}
				}
				Some(result)
			},
		}
	}

	pub fn authors(&self) -> Option<&str> {
		match self {
			Mods::Default(x) => x.authors.as_ref().map(|x| x as _),
			Mods::WithCreditList(x) => x.authors.as_ref().map(|x| x as _),
		}
	}
}

impl<const VARIANT: usize> Serialize for Mods<VARIANT> {
	fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match self {
			Mods::Default(x) => x.serialize(serializer),
			Mods::WithCreditList(x) => x.serialize(serializer),
		}
	}
}

impl<'de, const VARIANT: usize> Deserialize<'de> for Mods<VARIANT> {
	fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		Ok(match VARIANT {
			0 => Self::Default(ModsDefault::deserialize(deserializer)?),
			1 => Self::WithCreditList(ModsWithCreditsList::deserialize(deserializer)?),
			_ => unreachable!()
		})
	}
}

#[derive(Debug, Serialize, Deserialize, Encode, Decode)]
struct ModsDefault {
	#[serde(rename = "modId")]
	mod_id: String,
	#[serde(rename = "version")]
	version: String,
	#[serde(rename = "displayName")]
	display_name: Option<String>,
	#[serde(rename = "logoFile")]
	logo_file: Option<String>,
	#[serde(rename = "description")]
	description: Option<String>,
	#[serde(rename = "displayURL")]
	display_url: Option<String>,
	#[serde(rename = "credits")]
	credits: Option<String>,
	#[serde(rename = "authors")]
	authors: Option<String>
}

#[derive(Debug, Serialize, Deserialize, Encode, Decode)]
struct ModsWithCreditsList {
	#[serde(rename = "modId")]
	mod_id: String,
	#[serde(rename = "version")]
	version: String,
	#[serde(rename = "displayName")]
	display_name: Option<String>,
	#[serde(rename = "logoFile")]
	logo_file: Option<String>,
	#[serde(rename = "description")]
	description: Option<String>,
	#[serde(rename = "displayURL")]
	display_url: Option<String>,
	#[serde(rename = "credits")]
	credits: Option<Vec<String>>,
	#[serde(rename = "authors")]
	authors: Option<String>
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Copy, Encode, Decode)]
pub enum Side {
	#[serde(rename = "CLIENT")]
	Client,
	#[serde(rename = "SERVER")]
	Server,
	#[serde(rename = "BOTH")]
	Both
}

fn side_both() -> Side {
	Side::Both
}

#[derive(Debug, Serialize, Deserialize)]
struct Dependencies {
	#[serde(rename = "modId")]
	mod_id: String,
	#[serde(rename = "mandatory")]
	mandatory: bool,
	#[serde(rename = "versionRange")]
	version_range: Option<String>,
	#[serde(rename = "side")]
	#[serde(default = "side_both")]
	side: Side
}