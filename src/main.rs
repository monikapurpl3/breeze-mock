//! breeze-mock: mock Breeze Core servers and emulated Midea air conditioners,
//! described in XML, for testing and debugging.

use breeze_mock::blueprint;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: breeze-mock <command> [arguments]

  check <blueprint.xml>     validate a blueprint and summarise it
  version                   print the version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") if args.len() == 2 => check(PathBuf::from(&args[1])),
        Some("version") | Some("--version") if args.len() == 1 => {
            println!("breeze-mock {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help") | Some("--help") | Some("-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn check(path: PathBuf) -> ExitCode {
    match blueprint::load(&path) {
        Ok(bp) => {
            println!(
                "{}: blueprint {:?} is valid: {} segment(s), {} unit(s), {} server(s)",
                path.display(),
                bp.name,
                bp.segments.len(),
                bp.units.len(),
                bp.servers.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
