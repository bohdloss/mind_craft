#![feature(try_blocks)]
#![feature(gen_blocks)]
#![feature(let_chains)]

use std::fs::{File, OpenOptions};
use std::net::TcpListener;
use std::sync::{Arc, RwLock};
use std::thread;

use anyhow::{Context, Result};
use expanduser::expanduser;
use file_guard::{FileGuard, Lock};
use sha2::Sha256;
use yapper::{dispatch_debug, dispatch_display, Status};
use yapper::conf::Config;
use crate::client_loop::client_loop;
use crate::config::{SVManage};
use crate::ctxt::Ctxt;
use crate::server_loop::{Command, Server};

mod config;
mod ctxt;
mod server_loop;
mod client_loop;
mod sv_fs;

const LOCK: &str = "~/.sv_manage.lock";

fn acquire_lock() -> Result<FileGuard<Box<File>>> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(expanduser(LOCK).context("Failed to find home directory")?)
        .context("Failed to *open/create* the lock file")?;

    Ok(file_guard::try_lock(Box::new(lock), Lock::Exclusive, 0, isize::MAX as _)
        .context("Failed to *lock* the lock file")?)
}

fn main_wrapper() -> Result<()> {
    let lock = acquire_lock()
        .context("Failed to acquire lock")?;

    let config: Config<SVManage> = Config::init(config::CONFIG)
        .context("Failed to load configuration")?;
    
    let mut ctxt = Ctxt {
        lock,
        config,
        servers: Vec::new(),
    };

    let server = TcpListener::bind(format!("127.0.0.1:{}", ctxt.config.with_config(|x| x.gateway.port)))?;

    // Start up each server
    let names: Vec<String> = ctxt.config.with_config(|x| x.servers.keys().map(Clone::clone).collect());
    for name in names {
        let server = Server::new(name, ctxt.config.clone());
        ctxt.servers.push(server);
    }

    thread::scope(|s| {
        loop {
            if let Ok((client, _)) = server.accept() {
                s.spawn(|| {
                    if let Err(err) = client_loop(client, ctxt.config.clone(), &ctxt.servers) {
                        dispatch_debug(err);
                    }
                });
            }
        }
    });

    // let mut errors = Vec::with_capacity(ctxt.servers.len());
    // for server in ctxt.servers {
    //     errors.push(server.destroy());
    // }
    //
    // for error in errors {
    //     error?;
    // }

    Ok(())
}

fn main() -> core::result::Result<(), ()> {
    match main_wrapper() {
        Ok(()) => Ok(()),
        Err(err) => {
            println!("{:?}", err);
            Err(())
        }
    }
}