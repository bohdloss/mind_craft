#![feature(try_blocks)]
#![feature(let_chains)]

mod config;

use std::fs;
use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Mutex;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::http::header::{AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::{Router, serve};
use axum::routing::{get, MethodFilter, MethodRouter, post};
use http_body_util::StreamBody;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use yapper::conf::Config;
use anyhow::Result;
use axum::handler::HandlerWithoutStateExt;
use axum_server::tls_openssl::OpenSSLConfig;
use yapper::{DelOnDrop, DelOnDropOwned, dispatch_display};
use crate::config::{Access, CONFIG, Scope, SPAM, Token};

#[tokio::main]
async fn main() {
	let lock = config::acquire_lock().expect("Failed to acquire lock");
	let config: Config<SPAM> = Config::init(CONFIG).expect("Failed to load config");

	// config.with_config_mut(|conf| {
	// 	let uuid = Uuid::new_v4();
	// 	let mut token = Token::new();
	// 	token[Scope::Assets_Mods] = Access::Write | Access::Read;
	// 	conf.api_tokens.insert(uuid, token);
	// }).unwrap();

	let mods_router = Router::new()
		.route("/", post(post_mods))
		.route("/:unique_id", get(serve_mods))
		.layer(DefaultBodyLimit::max(1024 * 1024 * 1024));

	let asset_router = Router::new()
		.nest("/mods", mods_router);

	let base_router = Router::new()
		.route("/robots.txt", get(|| async { serve_file("text/plain; charset=utf-8", "./robots.txt").await }))
		.route("/", get(|| async { serve_file("text/html; charset=utf-8", "./index.html").await }))
		.route("/style.css", get(|| async { serve_file("text/css; charset=utf-8", "./style.css").await }))
		.route("/favicon.ico", get(|| async { serve_file("image/webp", "./icon.webp").await }))
		.nest("/assets", asset_router)
		.fallback(get(wtf_unknown_page))
		.with_state(config);

	axum_server::bind_openssl(include_str!("../ip.token").trim().parse().inspect_err(|err| dispatch_display(err)).unwrap(), OpenSSLConfig::from_pem(include_bytes!("../cert.pem"), include_bytes!("../key.pem")).unwrap())
		.serve(base_router.into_make_service())
		.await
		.expect("Serve failed");
}

async fn serve_file(content: impl AsRef<str>, path: impl AsRef<std::path::Path>) -> Response {
	let file = match File::open(path.as_ref()).await {
		Ok(file) => file,
		Err(err) => return wtf_unknown_page().await,
	};

	let name = path
		.as_ref()
		.file_name()
		.and_then(|x| x.to_str())
		.unwrap_or("document");

	file_response(file, content, name, true)
}

fn file_response<T: AsyncRead + Send + 'static>(file: T, c_type: impl AsRef<str>, filename: impl AsRef<str>, inline: bool) -> Response {
	if filename.as_ref().contains("\"") {
		return StatusCode::INTERNAL_SERVER_ERROR.into_response();
	}
	let stream = ReaderStream::new(file);
	let body = StreamBody::new(stream);

	let mut resp = Response::builder()
		.header(CONTENT_TYPE, c_type.as_ref());

	if inline {
		resp = resp.header(CONTENT_DISPOSITION, "inline");
	} else {
		resp = resp.header(CONTENT_DISPOSITION, format!(r#"attachment; filename="{}""#, filename.as_ref()));
	}

	resp.body(Body::from_stream(body))
		.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn wtf_unknown_page() -> Response {
	Response::builder()
		.status(StatusCode::NOT_FOUND)
		.body(Body::from("404 - Found, but I'm not showing u :P"))
		.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn post_mods(State(config): State<Config<SPAM>>, headers: HeaderMap, mut multipart: Multipart) -> Response {
	if let Some(header) = headers.get(AUTHORIZATION) &&
		let Ok(authorization) = header.to_str() &&
		let Some(("Bearer", token)) = authorization.split_once(" ") &&
		let Ok(uuid) = Uuid::from_str(token) &&
		config.with_config(|conf| {
			conf.api_tokens.get(&uuid).is_some_and(|token| token[Scope::Assets_Mods].contains(Access::Write))
		})
	{} else { return StatusCode::UNAUTHORIZED.into_response(); }
	let Ok(Some(mut field)) = multipart.next_field().await else { return StatusCode::BAD_REQUEST.into_response() };

	let Some("file") = field.name() else { return StatusCode::BAD_REQUEST.into_response() };
	
	let r: Result<(Uuid, DelOnDropOwned)> = try {
		let mut iter = 0;
		let (mut file, path, uuid) = loop {
			iter += 1;
			if iter >= 100 { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
			let uuid = Uuid::new_v4();
			let _ = fs::create_dir_all("./assets/mods");
			let path = PathBuf::from(format!("./assets/mods/{uuid}"));
			match File::create_new(&path).await {
				Ok(file) => break (file, path, uuid),
				Err(_) => continue,
			}
		};
		let del = DelOnDropOwned::new(path);
		while let Some(chunk) = field.chunk().await? {
			file.write_all(chunk.as_ref()).await?;
		}
		file.flush().await?;

		(uuid, del)
	};
	
	let Ok((uuid, del)) = r else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
	
	del.forgive();
	Response::builder()
		.header(CONTENT_TYPE, "text/plain")
		.header(CONTENT_DISPOSITION, "inline")
		.body(Body::from(uuid.to_string()))
		.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_mods(State(config): State<Config<SPAM>>, Path(unique_id): Path<String>) -> Response {
	let uuid = match Uuid::from_str(&unique_id) {
		Ok(uuid) => uuid,
		Err(err) => return wtf_unknown_page().await,
	};
	let _ = fs::create_dir_all("./assets/mods");

	let path = PathBuf::from(format!("./assets/mods/{uuid}"));
	let file = match File::open(&path).await {
		Ok(file) => file,
		Err(err) => return wtf_unknown_page().await,
	};
	let _ = fs::remove_file(&path); // The file is open, so removing it means we can still read it

	file_response(file, "application/zip", "mods.zip", false)
}