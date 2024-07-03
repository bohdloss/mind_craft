#![feature(try_blocks)]
#![feature(let_chains)]

use std::{io::stdin, sync::Arc};
use std::thread::spawn;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::RwLock;
use tokio::time::sleep;
use yapper::{NetCommand, Response, ServerCommand, Status};
use yapper::conf::Config;
use crate::comm::send_cmd;

mod bot;
mod comm;
mod conf;

/*
#[tokio::main]
async fn main() -> Result<()> {
    spawn(|| {
        loop {
            let err: Result<()> = try {
                let mut string = String::new();
                stdin().read_line(&mut string)?;
                let (op, server) = string.trim().split_once(" ").ok_or(anyhow!("WHAT"))?;
                let op = match op {
                    "start" => Ok(ServerCommand::Start),
                    "quit" => Ok(ServerCommand::Quit),
                    "reboot" => Ok(ServerCommand::Reboot),
                    "cmd" => {
                        let command = server.split(" ");
                        let mut string = String::new();
                        for x in command.skip(1) {
                            string.push_str(&format!("{x} "));
                        }

                        Ok(ServerCommand::Console(string))
                    }
                    _ => Err(anyhow!("Unknown operation")),
                }?;
                let (server, _) = server.split_once(" ").unwrap_or((server, ""));
                send_cmd!(NetCommand::ServerCommand(server.to_owned(), op) => Response::Ok => ())?;
            };
            if err.is_err() {
                println!("{err:?}");
            }
        }
    });

    let mut last = Status::Idle;
    loop {
        let err: Result<()> = try {
            let status = send_cmd!(NetCommand::ServerCommand("hotspot".to_owned(), ServerCommand::Status) => Response::Status(status) => status)?;
            if status != last {
                last = status;
                println!("{status}");
            }

            sleep(Duration::from_secs(1)).await;
        };
        if err.is_err() {
            println!("{err:?}");
        }
    }
}
*/

#[tokio::main]
async fn main() -> Result<()> {
    let lock = conf::acquire_lock()?;
    let config = Config::init(conf::CONFIG)?;

    bot::init(config).await?;

    Ok(())
}
