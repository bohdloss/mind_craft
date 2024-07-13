use std::collections::HashMap;
use std::fs::{File, FileType};
use std::{fs, io, process, thread};
use std::fmt::Debug;
use std::io::{ErrorKind, Read, Write};
use std::mem::replace;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use atomic::Atomic;
use base64::Engine;
use base64::engine::general_purpose;
use bytemuck::NoUninit;
use ende::{Decode, Encode, IntoRead};
use expanduser::expanduser;
use fs_extra::dir::{CopyOptions, TransitProcessResult};
use mc_rcon::RconClient;
use once_cell::sync::Lazy;
use parse_display::Display;
use reqwest::blocking::Client;
use reqwest::blocking::multipart::Form;
use reqwest::header::AUTHORIZATION;
use reqwest::StatusCode;
use zip::{CompressionMethod, ZipWriter};
use zip::write::SimpleFileOptions;
use uuid::Uuid;
use yapper::{base64_encode, DelOnDrop, dispatch_debug, ModInfo, Notification, parse_mod, Response, Status, ZipProgress};
use yapper::conf::Config;
use crate::config::{ServerConf, SVManage};
use crate::sv_fs;
use crate::sv_fs::Progress;

#[derive(Debug, Clone, Eq, PartialEq, Display, Encode, Decode)]
pub enum Command {
	DoNothing,
	#[display("Console({0:?})")]
	Console(String),
	Backup,
	Restore,
	#[display("ListMods({0:?}, {0:?})")]
	ListMods(u64, u64),
	#[display("InstallMod({0:?}, {0:?})")]
	InstallMod(String, String),
	#[display("UninstallMod({0:?})")]
	UninstallMod(String),
	#[display("UpdateMod({0:?}, {0:?})")]
	UpdateMod(String, String),
	#[display("QueryMod({0:?})")]
	QueryMod(String),
	GenerateModsZip,
}

struct ProcessComm {
	recv_cmd: Receiver<Command>,
	send_resp: Sender<Response>,
}

struct RequestComm {
	send_cmd: Sender<Command>,
	recv_resp: Receiver<Response>,
}

pub static NOTIFICATIONS: Lazy<Mutex<HashMap<String, Vec<Notification>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn get_notifs(account: impl AsRef<str>) -> Vec<Notification> {
	let mut notifs = NOTIFICATIONS.lock().unwrap();
	if !notifs.contains_key(account.as_ref()) {
		return Vec::new();
	}

	replace(&mut notifs.get_mut(account.as_ref()).unwrap(), Vec::new())
}

pub fn push_notif(account: impl AsRef<str>, notif: Notification) {
	let mut notifs = NOTIFICATIONS.lock().unwrap();
	if !notifs.contains_key(account.as_ref()) {
		notifs.insert(account.as_ref().to_owned(), Vec::new());
	}

	notifs.get_mut(account.as_ref()).unwrap().push(notif);
}

pub fn replace_notif_if<F>(account: impl AsRef<str>, notif: Notification, f: F)
where F: Fn(&Notification) -> bool
{
	let mut notifs = NOTIFICATIONS.lock().unwrap();
	if !notifs.contains_key(account.as_ref()) {
		notifs.insert(account.as_ref().to_owned(), Vec::new());
	}

	let vec = notifs.get_mut(account.as_ref()).unwrap();
	if let Some(other) = vec.iter_mut().find(|x| f(x)) {
		*other = notif;
		return;
	}
	vec.push(notif);
}

struct Shared {
	account: String,
	server: String,
	conf: Config<SVManage>,
	status_notifier: Condvar,
	status_notifier_lock: Mutex<()>,
	should_run: AtomicBool,
	reboot_queued: AtomicBool,
	status: Atomic<Status>,
	mods_up_to_date: AtomicBool,
}

impl Shared {
	pub fn update_status(&self, status: Status) {
		let old = self.status.swap(status, Ordering::AcqRel);
		if old != status {
			self.status_notifier.notify_all();
			push_notif(&self.account, Notification::StatusChanged(self.server.clone(), old, status));
		}
	}

	pub fn wait_for<F>(&self, mut predicate: F)
	where F: FnMut(Status) -> bool
	{
		let mut lock = self.status_notifier_lock.lock().unwrap();

		// Relaxed because acquiring the mutex already synchronizes the memory
		// every time
		while !predicate(self.status.load(Ordering::Relaxed)) {
			lock = self.status_notifier.wait(lock).unwrap();
		}
	}

	pub fn status(&self) -> Status {
		self.status.load(Ordering::Acquire)
	}
}

pub struct Server {
	handle: Mutex<Option<JoinHandle<()>>>,
	comm: Mutex<RequestComm>,
	shared: Arc<Shared>,
}

impl Server {
	pub fn new(account: String, name: String, conf: Config<SVManage>, server_conf: &ServerConf) -> Self {
		let (send_cmd, recv_cmd) = channel();
		let (send_resp, recv_resp) = channel();

		let comm = ProcessComm {
			recv_cmd,
			send_resp,
		};

		let should_run = server_conf.running;

		let shared = Arc::new(Shared {
			account: account.clone(),
			server: name.clone(),
			conf,
			status_notifier: Condvar::new(),
			status_notifier_lock: Mutex::new(()),
			should_run: AtomicBool::new(should_run),
			reboot_queued: AtomicBool::new(false),
			status: Atomic::new(Status::Idle),
			mods_up_to_date: AtomicBool::new(false),
		});
		let shared2 = shared.clone();

		Self {
			handle: {
				Mutex::new(Some(thread::spawn(move || server_main(account.clone(), name.clone(), comm, shared2))))
			},
			comm: Mutex::new(RequestComm {
				send_cmd,
				recv_resp,
			}),
			shared
		}
	}

	pub fn start(&self) {
		self.shared.should_run.store(true, Ordering::Release);
		let _ = self.shared.conf.with_config_mut(|x| {
			x.accounts.get_mut(self.account()).unwrap().servers.get_mut(self.name()).unwrap().running = true
		});
	}

	pub fn stop(&self) {
		self.shared.should_run.store(false, Ordering::Release);
		let _ = self.shared.conf.with_config_mut(|x| {
			x.accounts.get_mut(self.account()).unwrap().servers.get_mut(self.name()).unwrap().running = false
		});
	}

	pub fn reboot(&self) {
		self.start();
		self.shared.reboot_queued.store(true, Ordering::Release);
	}
	
	pub fn send(&self, cmd: Command, timeout: Duration) -> Result<Response> {
		let comm = self.comm.lock().unwrap();
		comm.send_cmd.send(cmd.clone())
			.with_context(|| format!("Failed to send {cmd} command"))?;

		let response = comm.recv_resp.recv_timeout(timeout)
			.context("Failed to receive response")?;

		Ok(response)
	}

	pub fn wait_for<F>(&self, predicate: F)
	where F: FnMut(Status) -> bool
	{
		self.shared.wait_for(predicate)
	}

	pub fn status(&self) -> Status {
		self.shared.status()
	}

	pub fn account(&self) -> &str {
		&self.shared.account
	}
	
	pub fn name(&self) -> &str {
		&self.shared.server
	}

	pub fn conf(&self) -> &Config<SVManage> {
		&self.shared.conf
	}
}

fn dispatch_err<T: Debug>(err: &T) { dispatch_debug(err) }

fn server_main(account: String, server: String, comm: ProcessComm, shared: Arc<Shared>) {
	let path = shared.conf.with_config(|x| x.accounts[&account].servers[&server].path.clone());

	let get_path = || -> Result<PathBuf> {
		Ok(expanduser(&path)
			.context("Failed to get home directory")?
			.canonicalize()
			.context("Failed to get absolute directory path")?)
	};

	let get_backup_path = || -> Result<PathBuf> {
		let path = get_path()?;

		let orig_name = path
			.file_name()
			.ok_or(anyhow!("Failed to get server folder name"))?
			.to_str()
			.ok_or(anyhow!("Failed to convert server folder name to utf8"))?;

		Ok(path
			.parent()
			.ok_or(anyhow!("Failed to get server parent folder"))?
			.join(format!("{orig_name}.bak")))
	};

	let get_mods_path = || -> Result<PathBuf> {
		let path = get_path()?.join("mods");
		ensure_dir(&path).context("Error creating mods folder")?;
		Ok(path)
	};

	loop {
		// Server was asked to quit
		// Wait for the user to change their mind
		// Keep gambling
		while !shared.should_run.load(Ordering::Acquire) {
			let command = match comm.recv_cmd.try_recv() {
				Ok(cmd) => Ok(Some(cmd)),
				Err(TryRecvError::Empty) => Ok(None),
				Err(err) => Err(err),
			}.unwrap();

			let mut deferred = Command::DoNothing;
			if let Some(command) = command {
				comm.send_resp.send(process_command_idle(command, &mut deferred, &shared, &get_path, &get_backup_path, &get_mods_path)
					.inspect_err(dispatch_err)
					.unwrap_or(Response::Err)).unwrap();
			}

			
			match deferred {
				Command::Backup => {
					shared.update_status(Status::BackingUp);
					let result: Result<()> = try {
						let working = get_path()?;
						let backup = get_backup_path()?;

						// Dont care if it doesn't exist
						let _ = fs::remove_dir_all(&backup);
						// We only care that now it must not exist anymore
						if backup.is_dir() { Err(anyhow!("Couldn't delete old backup directory"))?; }


						// Create the backup directory again
						fs::create_dir(&backup).context("Couldn't create backup directory")?;
						// This will take a long time!
						sv_fs::copy_dir_all(&working, &backup, |progress| {
							let new = Notification::BackupProgress(server.clone(), progress.copied, progress.total);
							replace_notif_if(&account, new, |other| other.is_backup_progress());
						}).context("Error copying data")?;
					};

					shared.update_status(Status::Idle);

					let _ = result.inspect_err(|err| {
						push_notif(&account, Notification::BackupFailed(server.clone(), format!("{err:?}")));
						dispatch_debug(err);
					});
				}
				Command::Restore => {
					let result: Result<()> = try {
						let working = get_path()?;
						let backup = get_backup_path()?;

						// Does a backup exist? It should at this point!
						if !backup.is_dir() { Err(anyhow!("No backup to restore"))?; }
						shared.update_status(Status::Restoring);
						// Time to delete the server directory
						fs::remove_dir_all(&working).context("Couldn't remove working server directory")?;

						// Create the server working directory again
						fs::create_dir(&working).context("Couldn't create working server directory")?;
						// This will take a long time!
						sv_fs::copy_dir_all(&backup, &working, |progress| {
							let mut notifications = NOTIFICATIONS.lock().unwrap();
							let new = Notification::RestoreProgress(server.clone(), progress.copied, progress.total);
							replace_notif_if(&account, new, |other| other.is_restore_progress());
						}).context("Error copying data")?;
					};

					shared.update_status(Status::Idle);

					let _ = result.inspect_err(|err| {
						push_notif(&account, Notification::RestoreFailed(server.clone(), format!("{err:?}")));
						dispatch_debug(err);
					});
				}
				Command::GenerateModsZip => {
					let result: Result<String> = try {
						if !shared.mods_up_to_date.load(Ordering::Acquire) {
							shared.update_status(Status::Packaging);
							let mods_folder = get_mods_path().context("Error getting mods folder path")?;
							gen_mods_zip(&mods_folder, &account, &server, &shared, &mut |progress| {
								replace_notif_if(&account, Notification::ZipProgress(server.clone(), ZipProgress::Zipping(
									progress.copied,
									progress.total
								)), |other| other.is_package_progress());
							}).context("Error generating mods zip file")?;
						}
						let url = upload_zip(&account, &server).context("Error uploading mods zip file")?;
						url
					};

					shared.update_status(Status::Idle);

					match result {
						Ok(url) => {
							push_notif(&account, Notification::ZipFile(server.clone(), url));
						}
						Err(err) => {
							push_notif(&account, Notification::ZipFailed(server.clone(), format!("{err:?}")));
							dispatch_debug(err);
						}
					}
				}
				_ => {}
			}

			sleep(Duration::from_millis(100));
		}

		// Run the server loop
		let _ = start_server(&get_path, &get_mods_path, &comm, &shared).inspect_err(dispatch_err);
	}
}

fn ensure_dir(path: &Path) -> Result<()> {
	match fs::create_dir(path) {
		Ok(_) => Ok(()),
		Err(err) if err.kind() == ErrorKind::AlreadyExists => {
			if path.is_file() {
				bail!("Path already exists and it's a file")
			}
			Ok(())
		},
		Err(err) => Err(err.into()),
	}
}

fn list_mods(path: &Path) -> Result<Vec<ModInfo>> {
	let mut vec = Vec::new();
	for entry in path.read_dir().context("Failed to list mods directory")? {
		let entry = entry.context("Failed to get directory listing item")?;
		if !entry.file_type().context("Failed to get item file type")?.is_file() {
			continue;
		}
		let path = entry.path();
		let Some(extension) = path.extension() else { continue };
		if extension != "jar" {
			continue;
		}

		// Parse mod
		let info = match parse_mod(&path).context(format!("while parsing {path:?}")) {
			Ok(info) => info,
			Err(err) => {
				dispatch_debug(err);
				continue
			}
		};
		vec.push(info);
	}
	vec.sort_by(|mod1, mod2| mod1.mod_id.cmp(&mod2.mod_id));
	Ok(vec)
}

fn query_mod(path: &Path, mod_id: &str) -> Result<ModInfo> {
	list_mods(path)?.into_iter().find(|x| &x.mod_id == mod_id).ok_or(anyhow!("Couldn't find {mod_id}"))
}

#[derive(Encode, Decode)]
struct Name {
	account: String,
	server: String
}

impl Name {
	pub fn new(account: impl Into<String>, server: impl Into<String>) -> Self {
		Self {
			account: account.into(),
			server: server.into(),
		}
	}

	pub fn from_base64(base64: impl AsRef<[u8]>) -> Self {
		let engine = general_purpose::URL_SAFE_NO_PAD;
		let data = engine.decode(base64).unwrap();
		ende::decode_bytes(&data).unwrap()
	}

	pub fn to_base64(&self) -> String {
		let engine = general_purpose::URL_SAFE_NO_PAD;
		let data = ende::encode_bytes(self).unwrap();
		engine.encode(&data)
	}
}

fn gen_mods_zip<F>(mods_folder: &Path, account: &str, server: &str, shared: &Shared, progress: &mut F) -> Result<PathBuf>
where F: FnMut(Progress)
{
	let name = Name::new(account, server);

	// Create the file
	let zip_path = PathBuf::from(format!("./{}.zip", name.to_base64()));

	if shared.mods_up_to_date.load(Ordering::Acquire) {
		return Ok(zip_path);
	}
	let total = fs_extra::dir::get_size(mods_folder).context("Failed to get total mod folder size")?;
	let mut copied = 0;
	
	let zip_file = File::create(&zip_path).context("Failed to create zip file")?;
	zip_file.set_len(0).context("Failed to truncate old zip file")?;

	let mut zip = ZipWriter::new(zip_file);
	let options = SimpleFileOptions::default()
		.compression_method(CompressionMethod::Deflated)
		.unix_permissions(0o755);

	let mut buffer = Vec::new();
	for entry in mods_folder.read_dir().context("Failed to list mods directory")? {
		let entry = entry.context("Failed to get directory listing item")?;
		if !entry.file_type().context("Failed to get item file type")?.is_file() {
			continue;
		}
		let path = entry.path();
		let Some(extension) = path.extension() else { continue };
		if extension != "jar" {
			continue;
		}
		let name = path.file_name()
			.ok_or(anyhow!("Directory entry doesn't have a file name!"))?
			.to_str()
			.ok_or(anyhow!("Couldn't convert filename to utf-8"))?;

		let size = entry.metadata()
			.context("Failed to get mod file metadata")?
			.len();
		
		zip.start_file(name, options).context("Failed to add file to zip")?;
		let mut mod_file = File::open(&path).context("Failed to open mod file for reading")?;
		mod_file.read_to_end(&mut buffer).context("Failed to read mod file to buffer")?;
		zip.write_all(&buffer).context("Failed to write mod file in the zip archive")?;
		
		copied += size;
		progress(Progress {
			copied,
			total
		});
			
		buffer.clear();
	}
	zip.finish().context("Failed to finalize zip file")?;

	shared.mods_up_to_date.store(true, Ordering::Release);
	Ok(zip_path)
}

fn upload_zip(account: &str, server: &str) -> Result<String>
{
	let name = Name::new(account, server);

	let zip_path = PathBuf::from(format!("./{}.zip", name.to_base64()));
	let zip_path = zip_path.canonicalize().context("Failed to canonicalize zip file path")?;

	let mut client = Client::builder().danger_accept_invalid_certs(true).build().unwrap();
	let response = client.post(format!("https://{}/assets/mods", include_str!("../ip.token").trim()))
		.bearer_auth(include_str!("../spam.token"))
		.multipart(Form::new().file("file", &zip_path).context("Failed to read mods zip file")?)
		.send()
		.context("Failed to post mods.zip")?;

	if response.status() == StatusCode::OK {
		let bytes = response.bytes().context("Failed to read response body")?;
		let string = core::str::from_utf8(bytes.as_ref()).context("Failed to read response body as utf-8")?;
		let uuid = Uuid::from_str(string).context("Response body wasn't a UUID")?;

		Ok(format!("https://{}/assets/mods/{uuid}", include_str!("../ip.token").trim()))
	} else {
		bail!("Failed to post mods.zip: {}", response.status())
	}
}

fn list_mods_paged<F>(per_page: u64, page: u64, mods: &F) -> Result<Response>
where F: Fn() -> Result<PathBuf>
{
	let mods_folder = mods()?;
	let mods = list_mods(&mods_folder)?;

	if per_page == 0 {
		Ok(Response::Mods(mods, true))
	} else {
		let per_page = per_page as usize;
		let page = page as usize;

		let offset = per_page * page;
		let end = offset + per_page;

		let offset = if offset > mods.len() { mods.len() } else { offset };
		let end = if end > mods.len() { mods.len() } else { end };

		let slice = if offset == end {
			&[]
		} else {
			&mods[offset..end]
		};

		let finish = (end - offset) < per_page;

		Ok(Response::Mods(slice.to_vec(), finish))
	}
}

fn install_mod<F>(current_path: String, filename: String, mods: &F, shared: &Shared) -> Result<Response>
where F: Fn() -> Result<PathBuf>
{
	let r: Result<Response> = try {
		let mod_path = PathBuf::from(current_path);
		let del = DelOnDrop::new(&mod_path);
		let mut filename = filename;
		if !filename.ends_with(".jar") {
			filename.push_str(".jar");
		}

		let mods_folder = mods()?.join("mods");
		ensure_dir(&mods_folder).context("Error creating mods folder")?;

		let to_install = parse_mod(&mod_path)?;
		let all = list_mods(&mods_folder)?;

		if all.iter().any(|modd| modd.mod_id == to_install.mod_id) {
			Response::ModConflict
		} else {
			shared.update_status(Status::Modding);
			let mut destination_path = mods_folder.join(&filename);
			loop {
				if !destination_path.exists() {
					break;
				}
				destination_path.set_file_name(format!("{filename}-2.jar"));
			};

			fs::rename(&mod_path, &destination_path).context("Failed to move mod")?;
			del.forgive();

			shared.mods_up_to_date.store(false, Ordering::Release);
			Response::Ok
		}
	};

	shared.update_status(Status::Idle);

	r
}

fn update_mod<F>(current_path: String, filename: String, mods: &F, shared: &Shared) -> Result<Response>
where F: Fn() -> Result<PathBuf>
{
	let r: Result<Response> = try {
		let mod_path = PathBuf::from(current_path);
		let del = DelOnDrop::new(&mod_path);
		let mut filename = filename;
		if !filename.ends_with(".jar") {
			filename.push_str(".jar");
		}

		let mods_folder = mods()?.join("mods");
		ensure_dir(&mods_folder).context("Error creating mods folder")?;

		let to_install = parse_mod(&mod_path)?;
		let all = list_mods(&mods_folder)?;

		if let Some(modd) = all.iter().find(|modd| modd.mod_id == to_install.mod_id) {
			shared.update_status(Status::Modding);
			let mut destination_path = mods_folder.join(&filename);
			loop {
				if !destination_path.exists() {
					break;
				}
				destination_path.set_file_name(format!("{filename}-2.jar"));
			};

			fs::remove_file(&modd.path).context("Failed to remove old mod file")?;
			fs::rename(&mod_path, &destination_path).context("Failed to move mod")?;
			del.forgive();

			shared.mods_up_to_date.store(false, Ordering::Release);
			Response::Ok
		} else {
			Response::NoSuchMod
		}
	};
	shared.update_status(Status::Idle);
	r
}

fn uninstall_mod<F>(mod_id: String, mods: &F, shared: &Shared) -> Result<Response>
where F: Fn() -> Result<PathBuf>
{
	let r: Result<Response> = try {

		let mods_folder = mods()?.join("mods");
		ensure_dir(&mods_folder).context("Error creating mods folder")?;

		let all = list_mods(&mods_folder)?;

		if let Some(modd) = all.iter().find(|modd| modd.mod_id == mod_id) {
			shared.update_status(Status::Modding);
			fs::remove_file(&modd.path).context("Error deleting mod file")?;

			shared.mods_up_to_date.store(false, Ordering::Release);
			Response::Ok
		} else {
			Response::NoSuchMod
		}
	};

	shared.update_status(Status::Idle);
	r
}

fn process_command_idle<F, G, H>(cmd: Command, deferred: &mut Command, shared: &Shared, path: &G, backup: &F, mods: &H) -> Result<Response>
where F: Fn() -> Result<PathBuf>,
      G: Fn() -> Result<PathBuf>,
      H: Fn() -> Result<PathBuf>
{
	match cmd {
		Command::Backup => {
			*deferred = Command::Backup;
			Ok(Response::Ok)
		},
		Command::Restore => {
			if !backup()?.is_dir() {
				Ok(Response::NoBackup)
			} else {
				*deferred = Command::Restore;
				Ok(Response::Ok)
			}
		},
		Command::ListMods(per_page, page) => {
			list_mods_paged(per_page, page, mods)
		}
		Command::QueryMod(mod_id) => {
			let mods_folder = mods()?;
			Ok(Response::Mod(query_mod(&mods_folder, &mod_id)?))
		}
		Command::InstallMod(mod_path, filename) => {
			install_mod(mod_path, filename, mods, shared)
		}
		Command::UpdateMod(mod_path, filename) => {
			update_mod(mod_path, filename, mods, shared)
		}
		Command::UninstallMod(mod_id) => {
			uninstall_mod(mod_id, mods, shared)
		}
		Command::GenerateModsZip => {
			*deferred = Command::GenerateModsZip;
			Ok(Response::Ok)
		}
		_ => Ok(Response::InvalidState),
	}
}

fn start_server<F: Fn() -> Result<PathBuf>, G: Fn() -> Result<PathBuf>>(path: &F, mods: &G, comm: &ProcessComm, shared: &Shared) -> Result<()> {
	// Start the server
	let runner = path()?.join("run.sh");
	println!("{:?}", runner);

	'start: while shared.should_run.load(Ordering::Acquire) {
		shared.reboot_queued.store(false, Ordering::Release);
		shared.update_status(Status::Starting);

		let props: Result<HashMap<String, String>> = try {
			let props_path = path()?.join("server.properties");

			let mut props_file = File::open(&props_path).context("Failed to open file")?;

			java_properties::read(&mut props_file).context("Failed to parse file")?
		};
		let props = props.context("Failed to load server.properties file").inspect_err(dispatch_err);
		
		#[derive(Debug)]
		struct RconConfig {
			port: u16,
			password: String,
		}

		let rcon_config = {
			if let Ok(props) = props &&
				let Some(enable_rcon) = props.get("enable-rcon") && enable_rcon.trim() == "true" &&
				let Some(rcon_port) = props.get("rcon.port") &&
				let Ok(rcon_port) = rcon_port.trim().parse() && 
				let Some(rcon_password) = props.get("rcon.password") && !rcon_password.trim().is_empty()
			{
				Some(RconConfig {
					port: rcon_port,
					password: rcon_password.clone()
				})
			} else { None }
		};

		let mut child = match process::Command::new(runner.clone())
			.stdin(Stdio::piped())
			.stdout(Stdio::inherit())
			.current_dir(path()?)
			.spawn()
			.context("Failed to start server") {
			Ok(child) => child,
			Err(err) => {
				dispatch_debug(err);
				sleep(Duration::from_secs(5));
				continue 'start;
			}
		};
		if rcon_config.is_none() {
			shared.update_status(Status::Running);
		}

		let mut rcon_client= None;

		'server: while shared.should_run.load(Ordering::Acquire) {
			if let Some(config) = &rcon_config && rcon_client.is_none() {
				let result: Result<RconClient> = try {
					let client = RconClient::connect(format!("127.0.0.1:{}", config.port))
					.context("Error connecting to RCON")?;
					client.log_in(&config.password)
					.context("Error logging into RCON")?;

					client
				};

				if let Ok(client) = result {
					shared.update_status(Status::Running);
					rcon_client = Some(client);
				}
			}
		
			if shared.reboot_queued.swap(false, Ordering::AcqRel) {
				break 'server;
			}
			let server_loop: Result<()> = try {
				let command = match comm.recv_cmd.try_recv() {
					Ok(cmd) => Ok(Some(cmd)),
					Err(TryRecvError::Empty) => Ok(None),
					Err(err) => Err(err),
				}?;
			
				if let Some(command) = command {
					comm.send_resp.send(process_command_inloop(command, &mut child, rcon_client.as_ref(), shared, path, mods)?)?;
				}
			
				if child.try_wait()?.is_some() { break 'server };
			};

			if let Err(err) = server_loop { dispatch_debug(err); break 'server; }
		}
		shared.update_status(Status::Stopping);
		cleanup(&mut child);
	}
	shared.update_status(Status::Idle);
	Ok(())
}

fn cleanup(child: &mut Child) {
	let result: Result<()> = try {
		match child.try_wait()? {
			None => {
				let start = SystemTime::now();
				let mut last_stop = SystemTime::now();
				
				let timeout = Duration::from_secs(5 * 60);
				let stop_interval = Duration::from_secs(5);
				loop {
					if child.try_wait()?.is_some() {
						break;
					}

					let now = SystemTime::now();
					
					if now.duration_since(start).unwrap() > timeout {
						Err(anyhow!("Timeout reached while stopping"))?;
					}
					
					if now.duration_since(last_stop).unwrap() > stop_interval {
						last_stop = now;
						let stdin = child.stdin.as_mut().unwrap();
						stdin.write_all("stop\n".as_bytes())?;
						stdin.flush()?;
					}
				}
				
				child.wait()?;
			}
			Some(_) => {}
		}
	};
	if result.is_err() {
		let _ = child.kill();
	}
}

fn process_command_inloop<F, G>(cmd: Command, child: &mut Child, client: Option<&RconClient>, shared: &Shared, path: &F, mods: &G) -> Result<Response>
where F: Fn() -> Result<PathBuf>,
      G: Fn() -> Result<PathBuf>
{
	match cmd {
		Command::Console(cmd) => {
			if let Some(client) = client {
				let output = client.send_command(&cmd)?;
				Ok(Response::CommandOutput(output))
			} else {
				let stdin = child.stdin.as_mut().unwrap();
				stdin.write_all(format!("{cmd}\n").as_bytes())?;
				stdin.flush()?;
				Ok(Response::CommandOutput("Command successfully sent, but output cannot be displayed because RCON is not enabled".to_string()))
			}
			
		},
		Command::ListMods(per_page, page) => {
			list_mods_paged(per_page, page, mods)
		}
		Command::QueryMod(mod_id) => {
			let mods_folder = mods()?;
			Ok(Response::Mod(query_mod(&mods_folder, &mod_id)?))
		}
		Command::InstallMod(mod_path, filename) => {
			install_mod(mod_path, filename, mods, shared)
		}
		Command::UpdateMod(mod_path, filename) => {
			update_mod(mod_path, filename, mods, shared)
		}
		Command::UninstallMod(mod_id) => {
			uninstall_mod(mod_id, mods, shared)
		}
		Command::GenerateModsZip => {
			shared.update_status(Status::Packaging);
			Ok(Response::Ok)
		}
		_ => Ok(Response::InvalidState),
	}
}