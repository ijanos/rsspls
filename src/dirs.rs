use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eyre::WrapErr;

use eyre::eyre;
use simple_eyre::eyre;

pub type Dirs = Arc<Mutex<BaseDirs>>;

pub struct BaseDirs;

pub fn new() -> eyre::Result<BaseDirs> {
    Ok(BaseDirs)
}

pub fn home_dir() -> Option<PathBuf> {
    ::dirs::home_dir()
}

impl BaseDirs {
    pub fn place_config_file<P: AsRef<Path>>(&self, path: P) -> eyre::Result<PathBuf> {
        let mut config =
            ::dirs::config_dir().ok_or_else(|| eyre!("unable to determine user config dir"))?;
        config.push("rsspls");
        fs::create_dir_all(&config)
            .wrap_err_with(|| format!("unable to create config directory: {}", config.display()))?;
        config.push(path);
        Ok(config)
    }

    pub fn place_cache_file<P: AsRef<Path>>(&self, path: P) -> eyre::Result<PathBuf> {
        let mut cache =
            ::dirs::cache_dir().ok_or_else(|| eyre!("unable to determine user cache dir"))?;
        cache.push("rsspls");
        fs::create_dir_all(&cache)
            .wrap_err_with(|| format!("unable to create cache directory: {}", cache.display()))?;
        cache.push(path);
        Ok(cache)
    }
}
