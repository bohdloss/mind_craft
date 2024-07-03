use std::collections::HashMap;
use std::fs::{File, FileType};
use std::{fs, process, thread};
use std::fmt::Debug;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use atomic::Atomic;
use bytemuck::NoUninit;
use ende::{Decode, Encode};
use expanduser::expanduser;
use fs_extra::dir::{CopyOptions, TransitProcessResult};
use mc_rcon::RconClient;
use parse_display::Display;
use yapper::{dispatch_debug, Notification, Response, Status};
use yapper::conf::Config;
use crate::config::{SVManage};
use crate::sv_fs;

#[derive(Debug, Clone, Eq, PartialEq, Display, Encode, Decode)]
pub enum Command {
	#[display("Console({0:?})")]
	Console(String),
	Backup,
	Restore,
}

struct ProcessComm {
	recv_cmd: Receiver<Command>,
	send_resp: Sender<Response>,
}

struct RequestComm {
	send_cmd: Sender<Command>,
	recv_resp: Receiver<Response>,
}

pub static NOTIFICATIONS: Mutex<Vec<Notification>> = Mutex::new(Vec::new());

struct Shared {
	name: String,
	conf: Config<SVManage>,
	status_notifier: Condvar,
	status_notifier_lock: Mutex<()>,
	should_run: AtomicBool,
	reboot_queued: AtomicBool,
	status: Atomic<Status>,
}

impl Shared {
	pub fn update_status(&self, status: Status) {
		let old = self.status.swap(status, Ordering::AcqRel);
		if old != status {
			self.status_notifier.notify_all();
			let mut notifs = NOTIFICATIONS.lock().unwrap();
			notifs.push(Notification::StatusChanged(self.name.clone(), old, status));
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
	pub fn new(name: String, conf: Config<SVManage>) -> Self {
		let (send_cmd, recv_cmd) = channel();
		let (send_resp, recv_resp) = channel();

		let comm = ProcessComm {
			recv_cmd,
			send_resp,
		};

		let should_run = conf.with_config(|x| x.servers[&name].running);

		let shared = Arc::new(Shared {
			name: name.clone(),
			conf,
			status_notifier: Condvar::new(),
			status_notifier_lock: Mutex::new(()),
			should_run: AtomicBool::new(should_run),
			reboot_queued: AtomicBool::new(false),
			status: Atomic::new(Status::Idle),
		});
		let shared2 = shared.clone();

		Self {
			handle: {
				Mutex::new(Some(thread::spawn(move || server_main(name.clone(), comm, shared2))))
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
		let _ = self.shared.conf.with_config_mut(|x| x.servers.get_mut(self.name()).unwrap().running = true);
	}

	pub fn stop(&self) {
		self.shared.should_run.store(false, Ordering::Release);
		let _ = self.shared.conf.with_config_mut(|x| x.servers.get_mut(self.name()).unwrap().running = false);
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

	pub fn name(&self) -> &str {
		&self.shared.name
	}

	pub fn conf(&self) -> &Config<SVManage> {
		&self.shared.conf
	}
}

fn dispatch_err<T: Debug>(err: &T) { dispatch_debug(err) }

fn server_main(name: String, comm: ProcessComm, shared: Arc<Shared>) {
	let path = shared.conf.with_config(|x| x.servers[&name].path.clone());

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

			if let Some(command) = command {
				comm.send_resp.send(process_command_idle(command, &shared, &get_backup_path).unwrap_or(Response::Err)).unwrap();
			}

			match shared.status() {
				Status::BackingUp => {
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
							let mut notifications = NOTIFICATIONS.lock().unwrap();
							let new = Notification::BackupProgress(name.clone(), progress.copied, progress.total);
							if let Some(present) = notifications.iter_mut().find(|x| x.is_backup_progress()) {
								*present = new;
							} else {
								notifications.push(new);
							}
						}).context("Error copying data")?;
					};

					shared.update_status(Status::Idle);

					let _ = result.inspect_err(|err| {
						NOTIFICATIONS.lock().unwrap().push(Notification::BackupFailed(name.clone(), format!("{err:?}")));
						dispatch_debug(err);
					});
				}
				Status::Restoring => {
					let result: Result<()> = try {
						let working = get_path()?;
						let backup = get_backup_path()?;

						// Does a backup exist? It should at this point!
						if !backup.is_dir() { Err(anyhow!("No backup to restore"))?; }
						// Time to delete the server directory
						fs::remove_dir_all(&working).context("Couldn't remove working server directory")?;

						// Create the server working directory again
						fs::create_dir(&working).context("Couldn't create working server directory")?;
						// This will take a long time!
						sv_fs::copy_dir_all(&backup, &working, |progress| {
							let mut notifications = NOTIFICATIONS.lock().unwrap();
							let new = Notification::RestoreProgress(name.clone(), progress.copied, progress.total);
							if let Some(present) = notifications.iter_mut().find(|x| x.is_restore_progress()) {
								*present = new;
							} else {
								notifications.push(new);
							}
						}).context("Error copying data")?;
					};

					shared.update_status(Status::Idle);

					let _ = result.inspect_err(|err| {
						NOTIFICATIONS.lock().unwrap().push(Notification::RestoreFailed(name.clone(), format!("{err:?}")));
						dispatch_debug(err);
					});
				}
				_ => {}
			}

			sleep(Duration::from_millis(100));
		}

		// Run the server loop
		let _ = start_server(&get_path, &comm, &shared).inspect_err(dispatch_err);
	}
}

fn process_command_idle<F: Fn() -> Result<PathBuf>>(cmd: Command, shared: &Shared, backup: &F) -> Result<Response> {
	match cmd {
		Command::Backup => {
			shared.update_status(Status::BackingUp);
			Ok(Response::Ok)
		},
		Command::Restore => {
			if !backup()?.is_dir() {
				Ok(Response::NoBackup)
			} else {
				shared.update_status(Status::Restoring);
				Ok(Response::Ok)
			}
		},
		_ => Ok(Response::InvalidState),
	}
}

fn start_server<F: Fn() -> Result<PathBuf>>(path: &F, comm: &ProcessComm, shared: &Shared) -> Result<()> {
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
					comm.send_resp.send(process_command_inloop(command, &mut child, rcon_client.as_ref(), shared)?)?;
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

fn process_command_inloop(cmd: Command, child: &mut Child, client: Option<&RconClient>, shared: &Shared) -> Result<Response> {
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
		_ => Ok(Response::InvalidState),
	}
}