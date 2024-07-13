#![feature(try_blocks)]

pub mod conf;

use std::collections::HashMap;
use std::fmt::Formatter;
use std::io::{Read, Write};
use std::mem;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use anyhow::{anyhow, bail, Result};
use base64::Engine;
use base64::engine::general_purpose;
use bytemuck::NoUninit;
use ende::{Context, Decode, Encode, Encoder};
use ende::io::{SizeLimit, Std, VecStream};
use openssl::rand::rand_bytes;
use openssl::symm;
use openssl::symm::Cipher;
use parse_display::Display;
use serde::{Deserialize, Serialize};
use sha2::digest::typenum::private::Trim;
use sha2::Sha256;

pub fn base64_encode<T: AsRef<[u8]>>(t: T) -> String {
	let encoder = general_purpose::URL_SAFE;
	encoder.encode(t)
}

pub fn base64_decode(t: &str) -> Vec<u8> {
	let encoder = general_purpose::URL_SAFE;
	encoder.decode(t).unwrap()
}

pub fn pretty_status(status: Status) -> String {
	match status {
		Status::Idle => ":zzz: **Idle**",
		Status::Running => ":white_check_mark: **Running**",
		Status::Stopping => ":octagonal_sign: **Stopping**",
		Status::Starting => ":stopwatch: **Starting**",
		Status::BackingUp => ":floppy_disk: **Creating backup**",
		Status::Restoring => ":leftwards_arrow_with_hook: **Restoring backup**",
		Status::Modding => ":stopwatch: **Modding**",
		Status::Packaging => ":package: **Packaging**",
	}.to_string()
}

pub fn escape_discord<T: core::fmt::Display>(display: T) -> String {
	let mut string = display.to_string();

	// Replace "\" with double "\\" to escape them properly in a discord message
	string = string.replace("\\", "\\\\");

	// Replace newline and cr
	string = string.replace("\n", "\\n*");
	string = string.replace("\r", "\\r");

	// Escape other chars
	string = string.replace("*", "\\*");
	string = string.replace("~", "\\~");
	string = string.replace("`", "\\`");
	string = string.replace("#", "\\#");
	string = string.replace("-", "\\-");
	string = string.replace(">", "\\>");
	string = string.replace(":", "\\:");

	// Parentheses
	string = string.replace("[", "\\[");
	string = string.replace("]", "\\]");
	string = string.replace("(", "\\(");
	string = string.replace(")", "\\)");

	string
}

pub fn dispatch_display<T: core::fmt::Display>(t: T) {
	println!("{t}");
}

pub fn dispatch_debug<T: core::fmt::Debug>(t: T) {
	println!("{t:?}")
}

pub fn hash_pw(pw: &str) -> [u8; 32] {
	use sha2::Digest;
	let mut hasher = Sha256::new();
	hasher.update(pw.as_bytes());
	let out = &hasher.finalize()[..];
	let mut ret = [0u8; 32];
	for (i, &byte) in out.iter().enumerate() {
		ret[i] = byte;
	}
	ret
}

pub trait Packet: Encode<VecStream> + Decode<VecStream> {
	type Response: PacketResponse;
}
pub trait PacketResponse: Encode<VecStream> + Decode<VecStream> {}

#[derive(Debug, Clone, Eq, PartialEq, Display, Encode, Decode)]
pub enum Command {
	Notifications,
	ListServers,
	Start,
	Quit,
	Status,
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode)]
pub enum NetCommand {
	ListServers,
	ServerCommand(String, ServerCommand),
	Notifications
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode)]
pub enum ServerCommand {
	Start,
	Stop,
	Status,
	Reboot,
	Console(String),
	Backup,
	Restore,
	ListMods(u64, u64),
	QueryMod(String),
	InstallMod(String, String),
	UninstallMod(String),
	UpdateMod(String, String),
	GenerateModsZip,
}

impl Packet for NetCommand {
	type Response = Response;
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode, Serialize, Deserialize)]
pub enum Notification {
	BackupFailed(String, String),
	RestoreFailed(String, String),
	StatusChanged(String, Status, Status),
	BackupProgress(String, u64, u64),
	RestoreProgress(String, u64, u64),
	ZipProgress(String, ZipProgress),
	ZipFailed(String, String),
	ZipFile(String, String),
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode, Serialize, Deserialize)]
pub enum ZipProgress {
	Zipping(u64, u64),
	Uploading(u64, u64),
}

impl core::fmt::Display for ZipProgress {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		match self {
			ZipProgress::Zipping(copied, total) => {
				write!(f,
				       "{:.2}Mb packaged out of {:.2}Mb ({:.2}%)",
				       *copied as f32 / (1024.0 * 1024.0),
				       *total as f32 / (1024.0 * 1024.0),
				       (*copied as f32 / *total as f32) * 100.0,
				)
			}
			ZipProgress::Uploading(copied, total) => {
				write!(f,
				       "{:.2}Mb uploaded out of {:.2}Mb ({:.2}%)",
				       *copied as f32 / (1024.0 * 1024.0),
				       *total as f32 / (1024.0 * 1024.0),
				       (*copied as f32 / *total as f32) * 100.0,
				)
			}
		}
	}
}

impl Notification {
	pub fn is_backup_progress(&self) -> bool {
		match self {
			Notification::BackupProgress(..) => true,
			_ =>  false,
		}
	}

	pub fn is_restore_progress(&self) -> bool {
		match self {
			Notification::RestoreProgress(..) => true,
			_ =>  false,
		}
	}

	pub fn is_package_progress(&self) -> bool {
		match self {
			Notification::ZipProgress(..) => true,
			_ =>  false,
		}
	}
}

impl core::fmt::Display for Notification {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		match self {
			Notification::BackupFailed(server, error) => {
				write!(f, "Backup failed for `{}` with error: {error}", escape_discord(server))
			}
			Notification::RestoreFailed(server, error) => {
				write!(f, "Restore failed for `{}` with error: {error}", escape_discord(server))
			}
			Notification::StatusChanged(server, old_status, new_status) => {
				write!(f, "Server `{}` is {}", escape_discord(server), pretty_status(*new_status))
			}
			Notification::BackupProgress(server, copied, total) => {
				write!(f,
				       "Server `{}` backup progress: {:.2}Mb copied out of {:.2}Mb ({:.2}%)",
				       escape_discord(server),
				       *copied as f32 / (1024.0 * 1024.0),
				       *total as f32 / (1024.0 * 1024.0),
				       (*copied as f32 / *total as f32) * 100.0,
				)
			}
			Notification::RestoreProgress(server, copied, total) => {
				write!(f,
				       "Server `{}` restore progress: {:.2}Mb copied out of {:.2}Mb ({:.2}%)",
				       escape_discord(server),
				       *copied as f32 / (1024.0 * 1024.0),
				       *total as f32 / (1024.0 * 1024.0),
				       (*copied as f32 / *total as f32) * 100.0,
				)
			}
			Notification::ZipProgress(server, progress) => {
				write!(f, 
				       "Server `{}` package progress: {progress}",
					   escape_discord(server)
				)
			}
			Notification::ZipFailed(server, error) => {
				write!(f,
				       "Package failed for  `{}` with error: {error}",
				       escape_discord(server)
				)
			}
			Notification::ZipFile(server, url) => {
				write!(f,
				       "Download ready for `{}`'s mod-pack: {url}",
				       escape_discord(server)
				)
			}
		}
	}
}

#[derive(Debug, Clone, Eq, PartialEq, Display, Encode, Decode)]
pub enum Response {
	Ok,
	Err,
	UnknownServer,
	InvalidState,
	NoBackup,
	#[display("Status({0}")]
	Status(ServerStatus),
	#[display("List({0:?})")]
	List(Vec<ServerStatus>),
	#[display("CommandOutput({0:?})")]
	CommandOutput(String),
	#[display("Notifications({0:?})")]
	Notifications(Vec<Notification>),
	ModConflict,
	NoSuchMod,
	#[display("Mods({0:?})")]
	Mods(Vec<ModInfo>, bool),
	#[display("Mod({0:?})")]
	Mod(ModInfo),
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode)]
pub struct ModInfo {
	pub filename: String,
	pub path: PathBuf,
	pub mod_id: String,
	pub name: Option<String>,
	pub description: Option<String>,
	pub version: Option<String>,
	pub logo: Option<Vec<u8>>,
	pub url: Option<String>,
	pub credits: Option<String>,
	pub authors: Option<Vec<String>>
}

pub fn parse_mod(path: &Path) -> Result<ModInfo> {
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

	let mut data: Result<ModsToml> = toml::from_str(&mods_toml).context("Failed to parse mods.toml");
	let mut data = match data {
		Ok(data) => data,
		_ => {
			let data: ModsTomlButCreditList = toml::from_str(&mods_toml).context("Failed to parse mods.toml (again)")?;
			let mut convert = Vec::new();
			for x in data.mods {
				let ModsButCreditsList {
					mod_id,
					version,
					display_name,
					logo_file,
					description,
					display_url,
					credits,
					authors,
				} = x;

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
				
				convert.push(Mods {
					mod_id,
					version,
					display_name,
					logo_file,
					description,
					display_url,
					credits,
					authors,
				})
			}
			ModsToml { mods: convert }
		}
	};

	if data.mods.len() != 1 {
		bail!(
            "Expected to find metadata for 1 mod but found metadata for {}",
            data.mods.len()
        );
	}

	let mut mods = data.mods.remove(0);

	let logo_data = if let Some(logo) = mods.logo_file {
		let x: Result<Vec<u8>> = try {
			let mut logo_file = archive
				.by_name(&logo)
				.context("Couldn't read logo file")?;
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

	let version = if let Some(version) = mods.version {
		let x: Result<String> = try {
			let mut the_version = version;
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
			}
			the_version
		};
		x.ok()
	} else { None };
	
	let filename: Result<String> = try {
		path
			.file_name()
			.ok_or(anyhow!("Couldn't get file name"))?
			.to_str()
			.ok_or(anyhow!("Couldn't convert to string"))?
			.to_string()
	};
	let filename = filename.unwrap_or(format!("{}.jar", mods.mod_id));

	let authors = mods.authors.map(|authors| {
		authors
			.split(",")
			.map(|string| string.trim())
			.map(|string| string.to_owned())
			.collect()
	});


	Ok(ModInfo {
		filename,
		path: path.canonicalize().context("Couldn't canonicalize path")?,
		mod_id: mods.mod_id,
		name: mods.display_name,
		description: mods.description,
		version,
		logo: logo_data,
		url: mods.display_url,
		credits: mods.credits,
		authors
	})
}

#[derive(Debug, Serialize, Deserialize)]
struct ModsTomlButCreditList {
	mods: Vec<ModsButCreditsList>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModsToml {
	mods: Vec<Mods>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModsButCreditsList {
	#[serde(rename = "modId")]
	mod_id: String,
	#[serde(rename = "version")]
	version: Option<String>,
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

#[derive(Debug, Serialize, Deserialize)]
struct Mods {
	#[serde(rename = "modId")]
	mod_id: String,
	#[serde(rename = "version")]
	version: Option<String>,
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

#[repr(transparent)]
pub struct DelOnDropOwned(PathBuf);

impl DelOnDropOwned {
	pub fn new(path: PathBuf) -> Self {
		Self(path)
	}

	pub fn forgive(self) {
		// Safety
		// This is safe because Self is repr(transparent) with a PathBuf
		let _path: PathBuf = unsafe { mem::transmute(self) };
	}
}

impl Drop for DelOnDropOwned {
	fn drop(&mut self) {
		let _ = std::fs::remove_file(&self.0);
	}
}

#[repr(transparent)]
pub struct DelOnDrop<'a>(&'a Path);

impl<'a> DelOnDrop<'a> {
	pub fn new(path: &'a Path) -> Self {
		Self(path)
	}

	pub fn forgive(self) {
		// No memory is leaked because we only store a reference
		mem::forget(self)
	}
}

impl Drop for DelOnDrop<'_> {
	fn drop(&mut self) {
		let _ = std::fs::remove_file(self.0);
	}
}

impl core::fmt::Display for ModInfo {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		core::fmt::Debug::fmt(self, f)
	}
}

impl PacketResponse for Response {}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Display, Encode, Decode, NoUninit, Serialize, Deserialize)]
#[repr(u8)]
pub enum Status {
	Idle,
	Starting,
	Running,
	Stopping,
	BackingUp,
	Restoring,
	Modding,
	Packaging
}

#[derive(Encode, Decode)]
pub struct PwMsg {
	hash: [u8; 32]
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode, Serialize, Deserialize)]
pub struct ServerStatus {
	pub name: String,
	pub path: String,
	pub status: Status,
}

impl core::fmt::Display for ServerStatus {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		core::fmt::Debug::fmt(self, f)
	}
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode)]
pub struct LoginPacket {
	pub user: String,
	pub password: [u8; 32],
}

impl Packet for LoginPacket {
	type Response = LoginResponse;
}

#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode)]
pub enum LoginResponse {
	Ok,
	WrongCredentials
}

impl PacketResponse for LoginResponse {}

pub fn send_packet<T>(client: &mut TcpStream, key: &[u8], ctxt: Context, packet: T)
	-> Result<T::Response>
where T: Packet,
{
	send_thing(client, key, ctxt, packet)?;
	recv_thing(client, key, ctxt)
}

fn send_thing<T: Encode<VecStream>>(client: &mut TcpStream, key: &[u8], ctxt: Context, t: T) -> Result<()> {
	// Encode to binary
	let vec = Vec::new();
	let mut encoder = Encoder::new(VecStream::new(vec, 0), ctxt);
	t.encode(&mut encoder)?;
	let mut vec = encoder.finish().0.into_inner();

	// Pad output
	let padding = 16 - (vec.len() % 16);
	for _ in 0..padding {
		vec.push(0);
	}
	// println!("SEND [PRE]: {vec:?}");
	
	// Gen IV
	let mut iv = [0u8; 16];
	rand_bytes(&mut iv)?;
	
	// Encrypt
	let output_crypt = symm::encrypt(Cipher::aes_128_cbc(), key, Some(&iv), &vec)?;

	// println!("SEND [POST]: {output_crypt:?}");
	
	// Write length to a buffer
	let len = (output_crypt.len() as u32).to_be_bytes();
	
	client.write_all(&iv)?;
	client.write_all(&len)?;
	client.write_all(&output_crypt)?;
	
	Ok(())
}

fn recv_thing<T: Decode<VecStream>>(client: &mut TcpStream, key: &[u8], ctxt: Context) -> Result<T> {
	let mut iv = [0u8; 16];
	client.read_exact(&mut iv)?;

	let mut len = [0u8; 4];
	client.read_exact(&mut len)?;
	let len = u32::from_be_bytes(len) as usize;

	// Decrypt the contents
	let mut vec = vec![0u8; len];
	client.read_exact(&mut vec)?;

	// println!("RECV [PRE]: {vec:?}");
	
	let decrypted_vec = symm::decrypt(Cipher::aes_128_cbc(), key, Some(&iv), &vec)?;

	// println!("RECV [POST]: {decrypted_vec:?}");
	
	// Decode
	let mut decoder = Encoder::new(VecStream::new(decrypted_vec, 0), ctxt);
	let decoded = T::decode(&mut decoder)?;

	Ok(decoded)
}

pub fn recv_packet<T, F, Err: Into<anyhow::Error>>(client: &mut TcpStream, key: &[u8], ctxt: Context, f: F)
                                               -> Result<()>
where T: Packet,
      F: FnOnce(T) -> core::result::Result<T::Response, (Err, T::Response)>,
{
	let msg = recv_thing(client, key, ctxt)?;
	match f(msg) {
		Ok(resp) => {
			send_thing(client, key, ctxt, resp)?;
			Ok(())
		}
		Err((err, resp)) => {
			send_thing(client, key, ctxt, resp)?;
			Err(err.into())
		}
	}
}