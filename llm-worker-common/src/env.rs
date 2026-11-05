//! Typed access to environment configuration with validation.
//!
//! Every getter takes the variable name so error messages say exactly which
//! variable is wrong. Empty values count as missing.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use color_eyre::eyre::{WrapErr as _, eyre};

/// A key → value lookup (the process environment, or a map in tests).
pub struct Env<'a> {
    get: &'a dyn Fn(&str) -> Option<String>,
}

impl<'a> Env<'a> {
    pub fn new(get: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Env { get }
    }

    fn get(&self, name: &str) -> Option<String> {
        (self.get)(name).filter(|v| !v.trim().is_empty())
    }

    pub fn required(&self, name: &str) -> color_eyre::Result<String> {
        self.get(name)
            .ok_or_else(|| eyre!("missing required env var: {name}"))
    }

    /// An `http(s)://` URL.
    pub fn url(&self, name: &str) -> color_eyre::Result<String> {
        let raw = self.required(name)?;
        let url = reqwest::Url::parse(raw.trim())
            .wrap_err_with(|| format!("{name} is not a valid URL"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(eyre!(
                "{name} must be an http(s) URL with a host, got {raw:?}"
            ));
        }
        Ok(raw.trim().to_string())
    }

    pub fn parse_or<T>(&self, name: &str, default: T) -> color_eyre::Result<T>
    where
        T: FromStr,
        T::Err: std::fmt::Display,
    {
        match self.get(name) {
            None => Ok(default),
            Some(v) => v
                .trim()
                .parse()
                .map_err(|e| eyre!("{name}={v:?} is invalid: {e}")),
        }
    }

    /// A positive integer (>= `min`).
    pub fn int_at_least<T>(&self, name: &str, default: T, min: T) -> color_eyre::Result<T>
    where
        T: FromStr + PartialOrd + std::fmt::Display + Copy,
        T::Err: std::fmt::Display,
    {
        let v = self.parse_or(name, default)?;
        if v < min {
            return Err(eyre!("{name} must be >= {min}, got {v}"));
        }
        Ok(v)
    }

    /// Whole seconds; `min_secs` is the smallest accepted value.
    pub fn secs(
        &self,
        name: &str,
        default_secs: u64,
        min_secs: u64,
    ) -> color_eyre::Result<Duration> {
        Ok(Duration::from_secs(self.int_at_least(
            name,
            default_secs,
            min_secs,
        )?))
    }

    pub fn path_or(&self, name: &str, default: &str) -> PathBuf {
        PathBuf::from(self.get(name).unwrap_or_else(|| default.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn required_rejects_missing_and_empty() {
        let m = env_of(&[("A", "x"), ("B", "  ")]);
        let get = |k: &str| m.get(k).cloned();
        let env = Env::new(&get);
        assert_eq!(env.required("A").unwrap(), "x");
        assert!(env.required("B").unwrap_err().to_string().contains("B"));
        assert!(env.required("C").unwrap_err().to_string().contains("C"));
    }

    #[test]
    fn url_validation() {
        let m = env_of(&[
            ("OK", "https://openrouter.ai/api/v1"),
            ("LOCAL", "http://127.0.0.1:8080/v1"),
            ("BAD", "not-a-url"),
            ("FTP", "ftp://example.com"),
        ]);
        let get = |k: &str| m.get(k).cloned();
        let env = Env::new(&get);
        assert!(env.url("OK").is_ok());
        assert!(env.url("LOCAL").is_ok());
        assert!(env.url("BAD").is_err());
        assert!(env.url("FTP").is_err());
    }

    #[test]
    fn numbers_and_bounds() {
        let m = env_of(&[("N", "5"), ("Z", "0"), ("X", "abc")]);
        let get = |k: &str| m.get(k).cloned();
        let env = Env::new(&get);
        assert_eq!(env.int_at_least("N", 1u32, 1).unwrap(), 5);
        assert_eq!(env.int_at_least("MISSING", 7u32, 1).unwrap(), 7);
        assert!(env.int_at_least("Z", 1u32, 1).is_err());
        assert!(env.int_at_least("X", 1u32, 1).is_err());
        assert_eq!(env.secs("Z", 30, 0).unwrap(), Duration::ZERO);
        assert!(env.secs("Z", 30, 1).is_err());
    }

    #[test]
    fn paths() {
        let m = env_of(&[("P", "/run/ready"), ("E", "")]);
        let get = |k: &str| m.get(k).cloned();
        let env = Env::new(&get);
        assert_eq!(env.path_or("P", "/tmp/ready"), PathBuf::from("/run/ready"));
        assert_eq!(env.path_or("E", "/tmp/ready"), PathBuf::from("/tmp/ready"));
        assert_eq!(
            env.path_or("MISSING", "/tmp/ready"),
            PathBuf::from("/tmp/ready")
        );
    }
}
