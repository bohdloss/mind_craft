use std::mem::replace;
use std::net::TcpStream;
use std::time::Duration;
use anyhow::{anyhow, Result};
use ende::{BitWidth, Context, Encoder};
use ende::io::Std;
use openssl::rsa::{Padding, Rsa};
use yapper::{LoginPacket, LoginResponse, NetCommand, recv_packet, Response, ServerCommand, ServerStatus, Status};
use yapper::conf::Config;
use crate::config::{GatewayConf, SVManage};
use crate::server_loop::{Command, NOTIFICATIONS, Server};

const KEY_PEM: &[u8] = include_bytes!("../sv_manage_private.pem");

pub fn client_loop(client: TcpStream, conf: Config<SVManage>, servers: &[Server]) -> Result<()> {
	let mut ctxt = Context::new();
	ctxt.settings.variant_repr.width = BitWidth::Bit128;
	// ctxt.settings.size_repr.num_encoding = NumEncoding::Leb128;
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

	recv_packet(&mut client, &aes, ctxt, |login: LoginPacket| {
		if login.user == "root" && login.password == conf.with_config(|x| x.gateway.pw_sha256) {
			Ok(LoginResponse::Ok)
		} else {
			Err((
				anyhow!(r#"Wrong credentials {:?}:{:?}"#, login.user, login.password),
				LoginResponse::WrongCredentials)
			)
		}
	})?;

	recv_packet(&mut client, &aes, ctxt, |command: NetCommand| {
		match &command {
			NetCommand::ListServers => {
				let mut list = Vec::with_capacity(servers.len());
				for server in servers {
					list.push(ServerStatus {
						name: server.name().to_owned(),
						path: server.conf().with_config(|x| x.servers[server.name()].path.clone()),
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
							match status {
								Status::Idle | Status::Stopping => {
									server.start();
									Ok(Response::Ok)
								}
								_ => Err((
									anyhow!("Trying to start with invalid status {status}"),
									Response::InvalidState,
								))
							}
						}
						ServerCommand::Stop => {
							match status {
								Status::Starting | Status::Running => {
									server.stop();
									Ok(Response::Ok)
								}
								_ => Err((
									anyhow!("Trying to quot with invalid status {status}"),
									Response::InvalidState,
								))
							}
						}
						ServerCommand::Status => {
							Ok(Response::Status(ServerStatus {
								name: server.name().to_owned(),
								path: server.conf().with_config(|x| x.servers[server.name()].path.clone()),
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
					}
				} else {
					Err((
						anyhow!(r#"Unknown server {:?}"#, s),
						Response::UnknownServer)
					)
				}
			}
			NetCommand::Notifications => {
				let notifs = replace(&mut *NOTIFICATIONS.lock().unwrap(), Vec::new());
				
				Ok(Response::Notifications(notifs))
			}
		}
	})?;

	Ok(())
}