//! `breeze-mock export`: what a real client needs to use a blueprint's world.
//!
//! - `breeze-core-config`: a `config.json` for a real Breeze Core, pointing at
//!   the emulated units with their tokens and keys. The tokens and keys are
//!   derived from the blueprint's seed, so the file keeps working across runs.
//! - `ca`: the certificate authority behind the blueprint's self-signed
//!   domains, for a client to trust.

use std::path::Path;

use sha2::Digest as _;

use crate::api::{backend, tls};
use crate::blueprint::{self, Blueprint};

fn load(path: &Path) -> Result<Blueprint, String> {
    blueprint::load(path).map_err(|e| e.to_string())
}

/// A `config.json` for a real Breeze Core: every unit, or those one server
/// manages (with that server's API key).
pub fn breeze_core_config(path: &Path, server: Option<&str>, out: Option<&Path>) -> Result<(), String> {
    let bp = load(path)?;
    let (units, key): (Vec<&blueprint::Unit>, String) = match server {
        Some(name) => {
            let s = bp
                .servers
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| format!("the blueprint has no server {name:?}"))?;
            let units = s
                .units
                .iter()
                .filter_map(|k| bp.units.iter().find(|u| &u.key == k))
                .collect();
            (units, s.api_key.clone())
        }
        None => {
            let digest = sha2::Sha256::digest(format!("{}/export/api-key", bp.seed));
            let key: String = digest[..24].iter().map(|b| format!("{b:02x}")).collect();
            (bp.units.iter().collect(), key)
        }
    };
    let config = backend::app_config(units, &key);
    match out {
        Some(p) => {
            breeze_store::save(p, &config, breeze_store::Mode::AdminReadable).map_err(|e| e.to_string())?;
            eprintln!(
                "wrote {} with {} unit(s); its API key is {key}",
                p.display(),
                config.units.len()
            );
        }
        None => println!("{}", breeze_store::to_json(&config).map_err(|e| e.to_string())?),
    }
    Ok(())
}

pub fn ca(path: &Path, out: Option<&Path>) -> Result<(), String> {
    let bp = load(path)?;
    let ca = tls::Ca::load_or_create(&crate::data::for_blueprint(&bp.name), &bp.name)?;
    match out {
        Some(p) => {
            std::fs::write(p, &ca.pem).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
            eprintln!("wrote {}", p.display());
        }
        None => print!("{}", ca.pem),
    }
    Ok(())
}
