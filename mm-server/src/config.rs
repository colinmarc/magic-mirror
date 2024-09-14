// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use anyhow::{bail, Context};
use lazy_static::lazy_static;
use regex::Regex;
use tracing::trace;

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    net::ToSocketAddrs,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

lazy_static! {
    static ref NAME_RE: Regex = Regex::new(r"\A[a-z][a-z0-9-_]{0,256}\z").unwrap();
    static ref DEFAULT_CFG: parsed::Config =
        toml::from_str(include_str!("../../mmserver.default.toml")).unwrap();
}

/// Serde representations of the configuration files.
mod parsed {
    use converge::Converge;
    use serde::Deserialize;
    use std::{collections::HashMap, num::NonZeroU32, path::PathBuf};

    #[derive(Debug, Clone, PartialEq)]
    pub(super) enum MaxConnections {
        Value(NonZeroU32),
        Infinity,
    }

    impl<'de> Deserialize<'de> for MaxConnections {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            #[serde(untagged, expecting = "a positive integer or \"inf\"")]
            enum Variant {
                Value(NonZeroU32),
                Infinity(f64),
            }

            match Deserialize::deserialize(deserializer)? {
                Variant::Value(n) => Ok(MaxConnections::Value(n)),
                Variant::Infinity(f) => {
                    if f.is_infinite() {
                        Ok(MaxConnections::Infinity)
                    } else {
                        Err(serde::de::Error::invalid_value(
                            serde::de::Unexpected::Float(f),
                            &"a positive integer or \"inf\"",
                        ))
                    }
                }
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Deserialize, Converge)]
    pub(super) struct Config {
        pub(super) include_apps: Option<Vec<PathBuf>>,
        pub(super) apps: Option<HashMap<String, AppConfig>>,

        #[converge(nest)]
        pub(super) server: Option<ServerConfig>,
        #[converge(nest)]
        pub(super) default_app_settings: Option<DefaultAppSettings>,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize, Converge)]
    #[serde(deny_unknown_fields)]
    pub(super) struct ServerConfig {
        pub(super) bind: Option<String>,
        pub(super) bind_systemd: Option<bool>,
        pub(super) tls_cert: Option<PathBuf>,
        pub(super) tls_key: Option<PathBuf>,
        pub(super) worker_threads: Option<NonZeroU32>,
        pub(super) max_connections: Option<MaxConnections>,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize, Converge)]
    #[serde(deny_unknown_fields)]
    pub(super) struct DefaultAppSettings {
        pub(super) xwayland: Option<bool>,
        pub(super) force_1x_scale: Option<bool>,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct AppConfig {
        pub(super) description: Option<String>,
        pub(super) command: Vec<String>,
        pub(super) environment: Option<HashMap<String, String>>,
        pub(super) xwayland: Option<bool>,
        pub(super) force_1x_scale: Option<bool>,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub server: ServerConfig,
    pub apps: HashMap<String, AppConfig>,
    pub bug_report_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub bind: String,
    pub bind_systemd: bool,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub worker_threads: NonZeroU32,
    pub max_connections: Option<NonZeroU32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub description: Option<String>,
    pub command: Vec<OsString>,
    pub env: HashMap<OsString, OsString>,
    pub xwayland: bool,
    pub force_1x_scale: bool,
}

impl Config {
    pub fn new(path: Option<&PathBuf>, includes: &[PathBuf]) -> anyhow::Result<Config> {
        let file = path
            .map(|p| p.to_owned())
            .or_else(locate_default_config_file);

        let cfg = if let Some(file) = file {
            let content = std::fs::read_to_string(&file)?;
            let parsed: parsed::Config = toml::from_str(&content)
                .context(format!("parsing configuration file {}", file.display()))?;

            Some(parsed)
        } else {
            None
        };

        let this = Self::build(cfg, includes)?;
        this.validate()?;

        Ok(this)
    }

    fn build(cfg: Option<parsed::Config>, includes: &[PathBuf]) -> anyhow::Result<Self> {
        // This is the parsed mmserver.defaults.toml.
        let defaults = DEFAULT_CFG.clone();

        let input = if let Some(cfg) = cfg {
            // Merge the default config with the input config, giving the input
            // precedence.
            use converge::Converge;
            cfg.converge(defaults)
        } else {
            defaults
        };

        // We only unwrap values that should have been set in the default
        // config. This is verified by a test.
        let server = input.server.unwrap();
        let default_app_settings = input.default_app_settings.unwrap();

        let mut this = Config {
            server: ServerConfig {
                bind: server.bind.unwrap(),
                bind_systemd: server.bind_systemd.unwrap(),
                tls_cert: server.tls_cert,
                tls_key: server.tls_key,
                worker_threads: server.worker_threads.unwrap(),
                max_connections: match server.max_connections.unwrap() {
                    parsed::MaxConnections::Value(n) => Some(n),
                    parsed::MaxConnections::Infinity => None,
                },
            },
            apps: HashMap::new(), // Handled below.
            bug_report_dir: None, // This is only set from the command line.
        };

        // Collect additional app definitions from app_dirs.
        let cfg_includes = input.include_apps.unwrap_or_default();

        let includes = cfg_includes.iter().chain(includes);
        let apps = input.apps.unwrap_or_default();

        let additional_apps = includes
            .map(|p| collect_includes(p).context(format!("searching {}", p.display())))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten();

        for (name, app) in apps.into_iter().chain(additional_apps) {
            if !NAME_RE.is_match(&name) {
                bail!("invalid app name: {}", name);
            }

            if this.apps.contains_key(&name) {
                bail!("duplicate app name: {}", name);
            }

            let res = AppConfig {
                description: app.description,
                command: app.command.into_iter().map(OsString::from).collect(),
                env: app
                    .environment
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                    .collect(),
                xwayland: app.xwayland.or(default_app_settings.xwayland).unwrap(),
                force_1x_scale: app
                    .force_1x_scale
                    .or(default_app_settings.force_1x_scale)
                    .unwrap(),
            };

            this.apps.insert(name, res);
        }

        trace!("using config: {:#?}", this);

        Ok(this)
    }

    /// Performs high-level validation on the final configuration.
    fn validate(&self) -> anyhow::Result<()> {
        if self.apps.is_empty() {
            bail!("at least one application must be defined");
        }

        for (name, app) in &self.apps {
            if app.command.is_empty() {
                bail!("empty command for application {:?}", name);
            }
        }

        let addr = self
            .server
            .bind
            .to_socket_addrs()
            .map(|mut addrs| addrs.next().unwrap())
            .map_err(|_| anyhow::anyhow!("invalid address \"{}\"", self.server.bind))?;

        // Check that TLS is set up (for non-private addresses).
        let ip = addr.ip();
        let tls_required = (ip_rfc::global(&ip) || ip.is_unspecified())
            && (self.server.tls_cert.is_none() || self.server.tls_key.is_none());
        if tls_required && (self.server.tls_cert.is_none() || self.server.tls_key.is_none()) {
            bail!("TLS required for non-private address \"{}\"", addr);
        }

        // Validate that the TLS cert and key exist.
        match self.server.tls_cert {
            Some(ref cert) if !cert.exists() => {
                bail!("TLS certificate not found at {}", cert.display());
            }
            _ => {}
        }

        match self.server.tls_key {
            Some(ref key) if !key.exists() => {
                bail!("TLS private key not found at {}", key.display());
            }
            _ => {}
        }

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Config::build(None, &[]).expect("failed to build default config")
    }
}

fn collect_includes(p: impl AsRef<Path>) -> anyhow::Result<Vec<(String, parsed::AppConfig)>> {
    let mut res = Vec::new();
    let p = p.as_ref();

    if !p.is_dir() {
        return Ok(vec![include_file(p)?]);
    }

    for entry in p.read_dir()? {
        let entry = entry?;

        match entry.file_type() {
            Ok(t) if t.is_file() => {
                let path = entry.path();
                let ext = path.extension().and_then(OsStr::to_str);
                if matches!(ext, Some("toml") | Some("json")) {
                    res.push(include_file(&path).context(format!("reading {}", path.display()))?)
                }
            }
            _ => continue,
        }
    }

    Ok(res)
}

fn include_file(p: impl AsRef<Path>) -> anyhow::Result<(String, parsed::AppConfig)> {
    let p = p.as_ref();
    let name = p
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow::anyhow!("invalid file name"))?;

    let content = std::fs::read_to_string(p)?;

    let app = match p.extension().and_then(OsStr::to_str) {
        Some("toml") => toml::from_str(&content)?,
        Some("json") => serde_json::from_str(&content)?,
        _ => bail!("invalid file extension"),
    };

    Ok((name.to_owned(), app))
}

fn locate_default_config_file() -> Option<PathBuf> {
    const BASENAME: &str = "/etc/magic-mirror/mmserver";

    for ext in &["toml", "json"] {
        let path = PathBuf::from(BASENAME).with_extension(ext);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

#[cfg(test)]
mod test {
    use super::*;
    use pretty_assertions::assert_eq;

    lazy_static! {
        static ref EXAMPLE_APP: AppConfig = AppConfig {
            description: None,
            command: vec!["echo".to_owned().into(), "hello".to_owned().into()],
            env: HashMap::new(),
            xwayland: true,
            force_1x_scale: false,
        };
    }

    fn config_from_str(s: &str) -> anyhow::Result<Config> {
        let input: parsed::Config = toml::from_str(s)?;
        Config::build(Some(input), &[])
    }

    #[test]
    fn test_default() {
        let mut config = Config::default();
        config
            .apps
            .insert("example".to_string(), EXAMPLE_APP.clone());

        config.validate().expect("default config is valid");
        assert_eq!(config.server.bind, "localhost:9599");
    }

    #[test]
    fn test_only_app() {
        let config = config_from_str(
            r#"
            [apps.example]
            command = ["echo", "hello"]
            "#,
        )
        .unwrap();

        config.validate().expect("empty config is valid");

        let mut expected = Config::default();
        expected
            .apps
            .insert("example".to_string(), EXAMPLE_APP.clone());

        assert_eq!(config, expected);
    }

    #[test]
    fn tls_required_for_global_addr() {
        let config = config_from_str(
            r#"
            [server]
            bind = "8.8.8.8:9599"
            [apps.example]
            command = ["echo", "hello"]
            "#,
        )
        .unwrap();

        eprintln!("{:?}", config.server);

        match config.validate() {
            Err(e) => {
                assert_eq!(
                    e.to_string(),
                    "TLS required for non-private address \"8.8.8.8:9599\""
                )
            }
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn tls_required_for_unspecified() {
        let config = config_from_str(
            r#"
            [server]
            bind = "[::]:9599"
            [apps.example]
            command = ["echo", "hello"]
            "#,
        )
        .unwrap();

        match config.validate() {
            Err(e) => {
                assert_eq!(
                    e.to_string(),
                    "TLS required for non-private address \"[::]:9599\""
                )
            }
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn tls_not_required_for_tailscale() {
        let config = config_from_str(
            r#"
            [server]
            bind = "100.64.123.45:9599"
            [apps.example]
            command = ["echo", "hello"]
            "#,
        )
        .unwrap();

        config
            .validate()
            .expect("TLS not required for shared NAT address");
    }
}
