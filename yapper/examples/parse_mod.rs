use std::{env, fs};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use anyhow::Result;
use zip::ZipArchive;
use yapper::parse_mod;

fn main() -> Result<()> {
	// let arg = env::args().nth(1).unwrap();
	// let the_one: usize = arg.parse()?;
	// let mut i: usize = 0;
	// for x in fs::read_dir("assets")? {
	// 	let x = x?;
	// 	println!("Parsing {:?}", x.path());
	// 	let modd = parse_mod(&x.path())?;
	// 	if i == the_one {
	// 		println!("{modd:?}");
	// 	}
	// 	
	// 	i += 1;
	// }
	
	// let x = parse_mod(&PathBuf::from("assets/create-1.19.2-0.5.1.f.jar"))?;
	// println!("{x:?}");
	finding_mc();
	Ok(())
}

fn finding_mc() -> Result<()> {
	walk_dir("../../../.minecraft")
}

fn walk_dir(dir: impl AsRef<Path>) -> Result<()> {
	const STRING: &str = "Minecraft, decompiled and deobfuscated with MCP technology";
	
	for item in fs::read_dir(dir)? {
		let item = item?;
		if item.file_type()?.is_dir() {
			walk_dir(item.path())?;
		}
		
		if item.file_type()?.is_file() {
			let Ok(mut zip) = ZipArchive::new(File::open(item.path())?) else {
				let mut string = String::new();
				let Ok(_) = File::open(item.path())?.read_to_string(&mut string) else { continue };
				if string.contains(STRING) {
					println!("{:?}", item.path());
				}
				continue
			};
			let names: Vec<String> = zip.file_names().map(|x| x.to_owned()).collect();
			for name in names.iter() {
				let Ok(mut file) = zip.by_name(name) else { continue };
				let mut string = String::new();
				let Ok(_) = file.read_to_string(&mut string) else { continue };
				if string.contains(STRING) {
					// println!("{string}");
					println!("{:?}", item.path());
				}
			}
		}
	}
	Ok(())
}