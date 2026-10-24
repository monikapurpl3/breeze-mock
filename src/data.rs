//! Where breeze-mock keeps what outlives a run: each blueprint's certificate
//! authority, and the stores of servers without a `state-dir=`.
//!
//! `BREEZE_MOCK_DATA` wins (the packaged service sets /var/lib/breeze-mock);
//! otherwise `%LOCALAPPDATA%\breeze-mock` on Windows and
//! `$XDG_DATA_HOME/breeze-mock` (or `~/.local/share/breeze-mock`) elsewhere.

use std::path::PathBuf;

pub fn root() -> PathBuf {
    if let Some(d) = std::env::var_os("BREEZE_MOCK_DATA").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if cfg!(windows) {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(d).join("breeze-mock");
        }
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("breeze-mock");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share/breeze-mock");
    }
    std::env::temp_dir().join("breeze-mock")
}

/// One blueprint's own directory, by its name.
pub fn for_blueprint(name: &str) -> PathBuf {
    root().join(name)
}

/// Create a directory readable only by its owner where the platform has modes.
pub fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write a file readable only by its owner where the platform has modes.
pub fn private_file(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
