use crate::bot::{DownloadedMod, FuckedUpMod, IdleMod, ModInstalling, MODS, ProcessedMod};
use anyhow::{anyhow, bail, Context, Result};
use image::DynamicImage;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use serenity::futures::{io, TryStreamExt};
use std::io::{Cursor, ErrorKind, Read};
use std::mem;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use serenity::all::GuildId;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tokio_util::io::StreamReader;
use yapper::{DelOnDrop, dispatch_debug, parse_mod};

pub async fn mod_thread(guild: GuildId, slot: usize) {
    let (server, att_name) = {
        let ModInstalling::Idle(IdleMod {
            server, att_name, ..
        }) = &MODS.lock().unwrap()[&guild][slot]
        else {
            panic!()
        };
        (server.clone(), att_name.clone())
    };
    match mod_thread_inner(guild, slot).await {
        Ok(_) => {}
        Err(err) => {
            dispatch_debug(&err);
            let _ = MODS.lock().unwrap().get_mut(&guild).unwrap().replace(
                slot,
                ModInstalling::FuckedUp(FuckedUpMod {
                    server,
                    att_name,
                    err: err.to_string(),
                }),
            );
        }
    }
}

async fn mod_thread_inner(guild: GuildId, slot: usize) -> Result<()> {
    let ModInstalling::Idle(IdleMod {
        server,
        att_name,
        url,
    }) = MODS.lock().unwrap()[&guild][slot].clone()
    else {
        return Err(anyhow!("Invalid mod state"));
    };

    let response = reqwest::get(url).await.context("URL get request failed")?;
    let stream = response
        .bytes_stream()
        .map_err(|err| std::io::Error::new(ErrorKind::BrokenPipe, "Unknown"));
    let reader = StreamReader::new(stream);

    let (file, path) = loop {
        let mut rand = rand::rngs::StdRng::from_entropy();
        let num = rand.gen_range(10000..100000);
        let path = PathBuf::from(format!("./mod_{num}_temp.bin"));

        match File::create_new(path.clone()).await {
            Ok(file) => break (file, path),
            _ => {}
        }
    };
    let del = DelOnDrop::new(&path);

    io::copy(reader.compat(), &mut file.compat_write())
        .await
        .context("Error downloading data")?;

    let new_state = ModInstalling::Downloaded(DownloadedMod {
        server: server.clone(),
        att_name: att_name.clone(),
        file: path.clone(),
    });

    MODS.lock().unwrap().get_mut(&guild).unwrap()[slot] = new_state;

    let mut info = parse_mod(&path)?;
    del.forgive();

    info.filename = att_name;
    let new_state = ModInstalling::Processed(ProcessedMod {
        server,
        info,
    });

    MODS.lock().unwrap().get_mut(&guild).unwrap()[slot] = new_state;

    Ok(())
}


