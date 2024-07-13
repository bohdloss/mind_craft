use std::env;
use std::env::args;

fn main() {
	let mut out = String::new();
	let mut first = true;
	for arg in env::args().skip(1) {
		if first {
			out.push_str(&arg);
			first = false;
		} else {
			out.push_str(&format!(" {arg}"));
		}
	}
	let hash = yapper::hash_pw(&out);
	println!("pw: {out:?}, hash: {hash:?}");
}