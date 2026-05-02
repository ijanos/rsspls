use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use eyre::WrapErr;

use eyre::eyre;
use simple_eyre::eyre;

const RSSPLS_HOME: &str = "RSSPLS_HOME";
const APP_DIR: &str = env!("CARGO_PKG_NAME");

pub fn home_dir() -> Option<PathBuf> {
    ::dirs::home_dir()
}

pub fn place_config_file<P: AsRef<Path>>(path: P) -> eyre::Result<PathBuf> {
    let mut config = config_dir()?;
    config.push(path);
    Ok(config)
}

pub fn place_cache_file<P: AsRef<Path>>(path: P) -> eyre::Result<PathBuf> {
    let mut cache = cache_dir()?;
    cache.push(path);
    Ok(cache)
}

pub fn default_output_dir() -> Option<PathBuf> {
    rsspls_home_dir().map(|path| path.join("out"))
}

fn config_dir() -> eyre::Result<PathBuf> {
    let config = if let Some(config) = rsspls_home_dir() {
        config
    } else {
        let mut config =
            ::dirs::config_dir().ok_or_else(|| eyre!("unable to determine user config dir"))?;
        config.push(APP_DIR);
        config
    };

    fs::create_dir_all(&config)
        .wrap_err_with(|| format!("unable to create config directory: {}", config.display()))?;
    Ok(config)
}

fn cache_dir() -> eyre::Result<PathBuf> {
    let cache = if let Some(mut cache) = rsspls_home_dir() {
        cache.push("cache");
        cache
    } else {
        let mut cache =
            ::dirs::cache_dir().ok_or_else(|| eyre!("unable to determine user cache dir"))?;
        cache.push(APP_DIR);
        cache
    };

    fs::create_dir_all(&cache)
        .wrap_err_with(|| format!("unable to create cache directory: {}", cache.display()))?;
    Ok(cache)
}

fn rsspls_home_dir() -> Option<PathBuf> {
    env::var_os(RSSPLS_HOME)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn rsspls_home_dir_uses_override_when_set() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            env::set_var(RSSPLS_HOME, "/tmp/rsspls-home-test");
        }

        assert_eq!(
            rsspls_home_dir(),
            Some(PathBuf::from("/tmp/rsspls-home-test"))
        );

        unsafe {
            env::remove_var(RSSPLS_HOME);
        }
    }

    #[test]
    fn rsspls_home_dir_treats_empty_override_as_unset() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            env::set_var(RSSPLS_HOME, "");
        }

        assert_eq!(rsspls_home_dir(), None);

        unsafe {
            env::remove_var(RSSPLS_HOME);
        }
    }
}
