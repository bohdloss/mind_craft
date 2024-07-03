use std::net::TcpStream;

use anyhow::{bail, Result};
use ende::io::Std;
use ende::{BitWidth, Context, Encoder};
use openssl::rsa::{Padding, Rsa};
use yapper::{hash_pw, send_packet, LoginPacket, LoginResponse, NetCommand, Response};

const IP: &str = "127.0.0.1:23786";

const PUB_KEY: &[u8] = include_bytes!("../sv_manage.pem");

macro_rules! send_cmd {
    ($cmd:expr => $resp:pat => $ret:expr) => {{
        match $crate::comm::send_command($cmd) {
            Ok($resp) => core::result::Result::Ok($ret),
            any => core::result::Result::Err(anyhow::anyhow!(
                "Expected {}, got {any:?}",
                stringify!($resp)
            )),
        }
    }};
}
pub(crate) use send_cmd;

pub fn send_command(cmd: NetCommand) -> Result<Response> {
    let client = TcpStream::connect(IP)?;
    let mut ctxt = Context::new();
    ctxt.settings.variant_repr.width = BitWidth::Bit128;
    // ctxt.settings.size_repr.num_encoding = NumEncoding::Leb128;
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

    let hash = hash_pw("My(NotSo)HardWorkByTheseWordsGuardedPlsDontHack");
    let login = LoginPacket {
        user: "root".to_string(),
        password: hash,
    };

    let login = send_packet(&mut client, &aes, ctxt, login)?;
    match login {
        LoginResponse::Ok => {}
        LoginResponse::WrongCredentials => bail!("Wrong credentials"),
    }

    let cmd = send_packet(&mut client, &aes, ctxt, cmd)?;

    Ok(cmd)
}
