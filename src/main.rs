//! breeze-mock: mock Breeze Core servers and emulated Midea air conditioners,
//! described in XML, for testing and debugging.

use breeze_mock::{blueprint, export, log, run};

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: breeze-mock <command> [arguments]

  check <blueprint.xml>                 validate a blueprint and summarise it
  run <blueprint.xml> [-v | -q]         run it until Ctrl-C: the emulated units, the
                                        mock servers and the console it describes
  export breeze-core-config <blueprint.xml> [--server NAME] [-o FILE]
                                        a config.json for a real Breeze Core, pointing
                                        at the emulated units (all, or one server's)
  export ca <blueprint.xml> [-o FILE]   the certificate authority behind the
                                        blueprint's self-signed domains, to trust
  version                               print the version

  -v shows every request and reply; -q only errors.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["check", path] => check(Path::new(path)),
        ["run", path, rest @ ..] => match rest {
            [] => run::run(Path::new(path)),
            ["-v"] => {
                log::set_level(2);
                run::run(Path::new(path))
            }
            ["-q"] => {
                log::set_level(0);
                run::run(Path::new(path))
            }
            _ => return usage(),
        },
        ["export", "breeze-core-config", path, rest @ ..] => {
            let Some((server, out)) = options(rest, true) else {
                return usage();
            };
            export::breeze_core_config(Path::new(path), server.as_deref(), out.as_deref())
        }
        ["export", "ca", path, rest @ ..] => {
            let Some((None, out)) = options(rest, false) else {
                return usage();
            };
            export::ca(Path::new(path), out.as_deref())
        }
        ["version"] | ["--version"] => {
            println!("breeze-mock {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["help"] | ["--help"] | ["-h"] => {
            print!("{USAGE}");
            Ok(())
        }
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("breeze-mock: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(2)
}

/// `--server NAME` (when allowed) and `-o FILE`, in either order.
fn options(rest: &[&str], server_allowed: bool) -> Option<(Option<String>, Option<PathBuf>)> {
    let (mut server, mut out) = (None, None);
    let mut it = rest.iter();
    while let Some(w) = it.next() {
        match *w {
            "--server" if server_allowed && server.is_none() => server = Some(it.next()?.to_string()),
            "-o" if out.is_none() => out = Some(PathBuf::from(it.next()?)),
            _ => return None,
        }
    }
    Some((server, out))
}

fn check(path: &Path) -> Result<(), String> {
    let bp = blueprint::load(path).map_err(|e| e.to_string())?;
    println!(
        "{}: blueprint {:?} is valid: {} segment(s), {} unit(s), {} server(s)",
        path.display(),
        bp.name,
        bp.segments.len(),
        bp.units.len(),
        bp.servers.len()
    );
    Ok(())
}
