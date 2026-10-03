//! `teleplot.yaml`.
//!
//! ```yaml
//! elasticsearch:
//!   url: https://es.example.com:9200   # or cloud_id: ...
//!   api_key: ${ES_API_KEY}              # or username / password
//!   ca_file: /etc/ssl/corp-ca.pem       # optional extra trusted CA
//! dashboards: ./dashboards              # Kibana .ndjson exports and Teleplot .yaml
//! time_zone: Europe/London              # for calendar buckets and date math
//! ```
//!
//! `${NAME}` anywhere in the file is replaced by that environment variable,
//! so secrets need not be written into it.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub elasticsearch: Elasticsearch,
    #[serde(default = "default_dashboards")]
    pub dashboards: PathBuf,
    /// IANA time zone; the system's if unset.
    #[serde(default)]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Elasticsearch {
    #[serde(default)]
    pub url: Option<String>,
    /// Elastic Cloud deployment ID, instead of `url`.
    #[serde(default)]
    pub cloud_id: Option<String>,
    /// Encoded API key, as Kibana and the API return it.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// PEM file with an extra certificate authority to trust.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    /// Skip TLS certificate verification. For local test clusters only.
    #[serde(default)]
    pub insecure: bool,
}

fn default_dashboards() -> PathBuf {
    PathBuf::from("dashboards")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text, |name| std::env::var(name).ok())
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str, env: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let text = interpolate(text, env)?;
        let mut config: Config = serde_yaml_ng::from_str(&text).map_err(|e| e.to_string())?;
        if config.elasticsearch.url.is_none() && config.elasticsearch.cloud_id.is_none() {
            return Err("elasticsearch needs a url or a cloud_id".into());
        }
        if let Some(tz) = &config.time_zone {
            jiff::tz::TimeZone::get(tz).map_err(|e| format!("time_zone {tz:?}: {e}"))?;
        }
        config.elasticsearch.url = config
            .elasticsearch
            .url
            .map(|u| u.trim_end_matches('/').to_string());
        Ok(config)
    }

    pub fn time_zone(&self) -> jiff::tz::TimeZone {
        self.time_zone
            .as_deref()
            .and_then(|tz| jiff::tz::TimeZone::get(tz).ok())
            .unwrap_or_else(jiff::tz::TimeZone::system)
    }
}

fn interpolate(text: &str, env: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or("unterminated ${ in config")?;
        let name = &after[..end];
        out.push_str(&env(name).ok_or_else(|| format!("environment variable {name} is not set"))?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_config_with_secrets_from_the_environment() {
        let c = Config::parse(
            "elasticsearch:\n  url: http://localhost:9200/\n  api_key: ${KEY}\ntime_zone: Europe/London\n",
            |name| (name == "KEY").then(|| "c2VjcmV0".to_string()),
        )
        .unwrap();
        assert_eq!(
            c.elasticsearch.url.as_deref(),
            Some("http://localhost:9200")
        );
        assert_eq!(c.elasticsearch.api_key.as_deref(), Some("c2VjcmV0"));
        assert_eq!(c.dashboards, PathBuf::from("dashboards"));
    }

    #[test]
    fn rejects_mistakes_early() {
        let none = |_: &str| None;
        assert!(
            Config::parse("elasticsearch: {}\n", none)
                .unwrap_err()
                .contains("url or a cloud_id")
        );
        assert!(
            Config::parse("elasticsearch:\n  url: x\n  api_key: ${MISSING}\n", none)
                .unwrap_err()
                .contains("MISSING")
        );
        assert!(
            Config::parse("elasticsearch:\n  url: x\ntime_zone: Mars/Olympus\n", none).is_err()
        );
        assert!(
            Config::parse("elasticsearch:\n  url: x\n  passwd: y\n", none).is_err(),
            "typos are errors"
        );
    }
}
