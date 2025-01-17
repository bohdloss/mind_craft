use std::net::TcpStream;

use anyhow::{bail, Result};
use ende::{BinSettings, BitWidth, Context, Encoder, NumEncoding, SizeRepr, VariantRepr};
use ende::io::Std;
use openssl::rsa::{Padding, Rsa};
use serenity::all::GuildId;

use yapper::{LoginPacket, LoginResponse, NetCommand, Response, send_packet};
use yapper::conf::Config;

use crate::bot::SharedMin;
use crate::conf::MCAYB;

const IP: &str = "127.0.0.1:23786";

const PUB_KEY: &[u8] = include_bytes!("../sv_manage.pem");

fn login(conf: &Config<MCAYB>, guild_id: GuildId) -> (String, [u8; 32]) {
    conf.with_config(|conf| {
        let ref data = conf.guild_data[&guild_id];
        (data.sv_user.clone(), data.sv_pass)
    })
}

pub fn send_command(shared: &SharedMin, cmd: NetCommand) -> Result<Response> {
    let client = TcpStream::connect(IP)?;
    let ctxt = Context::new()
        .settings(BinSettings::new()
            .variant_repr(VariantRepr::new()
                .bit_width(BitWidth::Bit8))
            .size_repr(SizeRepr::new()
                .num_encoding(NumEncoding::Leb128)));
    let mut encoder = Encoder::new(Std::new(client), ctxt);

    // Generate aes key
    let mut aes = [0u8; 16];
    openssl::rand::rand_bytes(&mut aes)?;

    // Encrypt it
    let key = Rsa::public_key_from_pem(PUB_KEY)?;
    let mut encrypted = [0u8; 256];
    key.public_encrypt(&aes, &mut encrypted, Padding::PKCS1)?;

    encoder.encode_value(encrypted)?;

    let mut client = encoder.finish().0.into_inner();

    // Begin packet exchange
    let (acc, pw) = login(&shared.conf, shared.guild);
    let login = LoginPacket {
        user: acc,
        password: pw,
    };

    let login = send_packet(&mut client, &aes, ctxt, login)?;
    match login {
        LoginResponse::Ok => {}
        LoginResponse::WrongCredentials => bail!("Wrong credentials"),
    }

    let cmd = send_packet(&mut client, &aes, ctxt, cmd)?;

    Ok(cmd)
}
