use std::mem::{MaybeUninit, replace};
use std::net::TcpStream;
use std::process;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use anyhow::{anyhow, Result};
use ende::{BinSettings, BitWidth, Context, Encoder, NumEncoding, SizeRepr, VariantRepr};
use ende::io::Std;
use openssl::rsa::{Padding, Rsa};
use yapper::{LoginPacket, LoginResponse, NetCommand, recv_packet, Response, ServerCommand, ServerStatus, Status};
use yapper::conf::Config;
use crate::config::{SVManage};
use crate::ctxt::Ctxt;
use crate::server_loop::{Command, get_notifs, NOTIFICATIONS, Server};

const KEY_PEM: &[u8] = include_bytes!("../sv_manage_private.pem");

pub fn client_loop(client: TcpStream, ctx: Arc<Ctxt>) -> Result<()> {
	let mut ctxt = Context::new()
		.settings(BinSettings::new()
			.variant_repr(VariantRepr::new()
				.bit_width(BitWidth::Bit8))
			.size_repr(SizeRepr::new()
				.num_encoding(NumEncoding::Leb128)));
	let mut encoder = Encoder::new(Std::new(client), ctxt);

	// Oh boy

	// Expect an aes key encrypted with our public key
	let buffer: [u8; 256] = encoder.decode_value()?;
	let key = Rsa::private_key_from_pem(KEY_PEM)?;

	let mut decrypted = [0u8; 256];
	key.private_decrypt(&buffer, &mut decrypted, Padding::PKCS1)?;

	// We got the key and iv
	let aes = &decrypted[..16];
	
	let mut client = encoder.finish().0.into_inner();

	// Packet exchange here

	let mut account_name = None;
	recv_packet(&mut client, &aes, ctxt, |login: LoginPacket| {
		ctx.config.with_config(|conf| {
			if let Some(account) = conf.accounts.get(&login.user) && account.password == login.password {
				account_name = Some(login.user);
				Ok(LoginResponse::Ok)
			} else {
				Err((
					anyhow!(r#"Wrong credentials {:?}:{:?}"#, login.user, login.password),
					LoginResponse::WrongCredentials)
				)
			}
		})
	})?;
	let account = account_name.unwrap();
	
	let ref servers = ctx.servers[&account];
	
	recv_packet(&mut client, &aes, ctxt, |command: NetCommand| {
		match &command {
			NetCommand::ListServers => {
				let mut list = Vec::with_capacity(servers.len());
				for server in servers.iter() {
					list.push(ServerStatus {
						name: server.name().to_owned(),
						path: server.conf().with_config(|x| x.accounts[&account].servers[server.name()].path.clone()),
						status: server.status(),
					});
				}

				Ok(Response::List(list))
			}
			NetCommand::ServerCommand(s, cmd) => {
				if let Some(server) = servers.iter().find(|x| x.name() == s) {
					let status = server.status();
					match cmd {
						ServerCommand::Start => {
							server.start();
							Ok(Response::Ok)
						}
						ServerCommand::Stop => {
							server.stop();
							Ok(Response::Ok)
						}
						ServerCommand::Status => {
							Ok(Response::Status(ServerStatus {
								name: server.name().to_owned(),
								path: server.conf().with_config(|x| x.accounts[&account].servers[server.name()].path.clone()),
								status: server.status()
							}))
						}
						ServerCommand::Reboot => {
							server.reboot();
							Ok(Response::Ok)
						}
						ServerCommand::Console(cmd) => {
							if status != Status::Running {
								return Err((
									anyhow!("Server not running, can't run command: {status}"),
									Response::InvalidState,
								))
							}
							
							use anyhow::Context;
							let x = server.send(Command::Console(cmd.clone()), Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::Backup => {
							if status != Status::Idle {
								return Err((
									anyhow!("Server not idle, can't backup: {status}"),
									Response::InvalidState,
								))
							}

							use anyhow::Context;
							let x = server.send(Command::Backup, Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::Restore => {
							if status != Status::Idle {
								return Err((
									anyhow!("Server not idle, can't restore: {status}"),
									Response::InvalidState,
								))
							}

							use anyhow::Context;
							let x = server.send(Command::Restore, Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::ListMods(per_page, pages) => {
							use anyhow::Context;
							let x = server.send(Command::ListMods(*per_page, *pages), Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::InstallMod(filename, preferred_name) => {
							use anyhow::Context;
							let x = server.send(Command::InstallMod(filename.clone(), preferred_name.clone()), Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::UninstallMod(mod_id) => {
							use anyhow::Context;
							let x = server.send(Command::UninstallMod(mod_id.clone()), Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::UpdateMod(filename, preferred_name) => {
							use anyhow::Context;
							let x = server.send(Command::UpdateMod(filename.clone(), preferred_name.clone()), Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::QueryMod(mod_id) => {
							use anyhow::Context;
							let x = server.send(Command::QueryMod(mod_id.clone()), Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::GenerateModsZip => {
							use anyhow::Context;
							let x = server.send(Command::GenerateModsZip, Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
						ServerCommand::ResolveDeps(mode, new_mods) => {
							use anyhow::Context;
							let x = server.send(Command::ResolveDeps(*mode, new_mods.clone()), Duration::from_secs(5))
								.context("Failed to send command")
								.map_err(|err| (err, Response::Err))?;
							Ok(x)
						}
					}
				} else {
					Err((
						anyhow!(r#"Unknown server {:?}"#, s),
						Response::UnknownServer)
					)
				}
			}
			NetCommand::Notifications => {
				let notifs = get_notifs(&account);
				
				Ok(Response::Notifications(notifs))
			}
		}
	})?;

	Ok(())
}