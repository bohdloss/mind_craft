use std::fs::File;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Test {
	x: Enum,
	c: u32,
	f: f32
}

#[derive(Serialize, Deserialize)]
enum Enum {
	Test1 {
		a: u32,
		b: u32,
	},
	Test2
}

fn main() {
    let x = Test {
	    // x: Enum::Test1 {
		//     a: 2,
		//     b: 5
	    // },
	    x: Enum::Test2,
	    c: 15,
	    f: 0.9f32
    };
	
	let file = File::create("./test.json").unwrap();
	serde_json::to_writer_pretty(file, &x).unwrap();
}