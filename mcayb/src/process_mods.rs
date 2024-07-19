use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rand::{Rng, SeedableRng};
use serenity::futures::{io, TryStreamExt};
use tokio::fs::File;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tokio_util::io::StreamReader;

use yapper::{DelOnDrop, dispatch_debug, parse_mod};

use crate::bot::{DownloadedMod, FuckedUpMod, IdleMod, ModInstalling, ProcessedMod, Shared};

pub async fn mod_thread(shared: Arc<Shared>, data: IdleMod) {
    let slot = shared.mods_allocate(ModInstalling::Idle(data.clone())).await;
    let server = data.server.clone();
    let att_name = data.att_name.clone();
    
    match mod_thread_inner(&shared, slot, data).await {
        Ok(_) => {}
        Err(err) => {
            dispatch_debug(&err);
            shared.mods_set(
                slot,
                ModInstalling::FuckedUp(FuckedUpMod {
                    server,
                    att_name,
                    err: err.to_string(),
                }),
            ).await;
        }
    }
}

async fn mod_thread_inner(shared: &Shared, slot: usize, data: IdleMod) -> Result<()> {
    let IdleMod {
        server,
        att_name,
        url,
    } = data;

    let response = reqwest::get(url).await.context("URL get request failed")?;
    let stream = response
        .bytes_stream()
        .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "Unknown"));
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

    shared.mods_set(slot, new_state).await;

    let mut info = parse_mod(&path)?;
    del.forgive();

    info.filename = att_name;
    let new_state = ModInstalling::Processed(ProcessedMod {
        server,
        info,
    });

    shared.mods_set(slot, new_state).await;

    Ok(())
}


