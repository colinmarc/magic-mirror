// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod codec;
mod color;
mod config;
mod container;
mod encoder;
mod pixel_scale;
mod server;
mod session;
mod state;
mod vulkan;
mod waking_sender;

use std::{
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use parking_lot::Mutex;
use tracing::{debug, info, warn};
use tracing_subscriber::{util::SubscriberInitExt, EnvFilter, Layer};

#[derive(Debug, Parser)]
#[command(name = "mmserver")]
#[command(about = "The Magic Mirror server", long_about = None)]
struct Cli {
    /// Print the version.
    #[arg(short, long)]
    version: bool,
    /// The address to bind. Defaults to [::0]:9599.
    #[arg(long, value_name = "HOST[:PORT]")]
    bind: Option<String>,
    /// Bind using systemd's socket passing protocol (LISTEN_FDS).
    #[arg(long)]
    bind_systemd: bool,
    /// The path to a config file. By default,
    /// /etc/magic-mirror/mmserver.{toml,json} is used (if present).
    #[arg(short = 'C', long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Include extra app definitions. May be specified multiple times, with
    /// either individual files or directories to be searched.
    #[arg(short = 'i', long, value_name = "PATH")]
    include_apps: Vec<PathBuf>,
    /// Generate a bug report in a temporary directory. WARNING: this will save
    /// video recordings, which may be large!
    #[arg(long)]
    bug_report: bool,
}

fn main() -> Result<()> {
    let args = Cli::parse();

    let version = format!(
        "mmserver {}",
        git_version::git_version!(
            args = ["--always", "--tags", "--match", "mmserver-v"],
            prefix = "git:",
            cargo_prefix = "",
        )
    );

    if args.version {
        println!("{}", version);
        return Ok(());
    }

    let bug_report_dir = if args.bug_report {
        let dirname = std::env::temp_dir().join(format!("magic-mirror-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o0755).create(&dirname)?;

        Some(dirname)
    } else {
        None
    };

    init_logging(bug_report_dir.as_ref())?;

    debug!(version, "starting up");
    if let Some(ref dirname) = bug_report_dir {
        warn!("generating bug report files in: {:?}", &dirname);
    }

    #[cfg(feature = "tracy")]
    warn!("tracing enabled!");

    // Load config.
    let mut cfg = config::Config::new(args.config.as_ref(), &args.include_apps)
        .context("failed to read config")?;

    let vk = Arc::new(vulkan::VkContext::new(cfg!(debug_assertions))?);

    preflight_checks(&cfg, &vk)?;

    // Override with command line flags.
    cfg.bug_report_dir = bug_report_dir.clone();
    if let Some(bind) = args.bind {
        cfg.server.bind = bind;
    } else if args.bind_systemd {
        cfg.server.bind_systemd = true;
    }

    let sock = if cfg.server.bind_systemd {
        let mut listenfd = listenfd::ListenFd::from_env();
        if let Some(sock) = listenfd.take_udp_socket(0)? {
            debug!("using systemd socket: {:?}", sock.local_addr()?);
            sock
        } else {
            bail!("systemd UDP socket not found")
        }
    } else {
        std::net::UdpSocket::bind(&cfg.server.bind).context("binding server socket")?
    };

    let state = Arc::new(Mutex::new(state::ServerState::new(vk, cfg.clone())));
    let mut srv = server::Server::new(sock, cfg.server.clone(), state)?;

    let closer = srv.closer();
    ctrlc::set_handler(move || {
        debug!("received SIGINT");
        closer.send(()).ok();
    })?;

    info!("listening on {:?}", srv.local_addr());
    srv.run().context("server exited")?;

    if let Some(dir) = &bug_report_dir {
        save_vulkaninfo(dir);
        info!("bug report files saved to: {:?}", dir);
    }

    Ok(())
}

fn init_logging(bug_report_dir: Option<impl AsRef<Path>>) -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;

    let trace_log = if let Some(dir) = bug_report_dir {
        // Additionally write a trace log with everything to the bug report dir.
        let file = std::fs::File::create(dir.as_ref().join("mmserver.log"))?;
        let trace_filter =
            tracing_subscriber::EnvFilter::new("mmserver=trace,fuser=trace,southpaw=trace");

        let trace_log = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .with_filter(trace_filter);

        Some(trace_log)
    } else {
        None
    };

    let tracy = if cfg!(feature = "tracy") {
        Some(tracing_tracy::TracyLayer::default().with_filter(EnvFilter::new("mmserver=trace")))
    } else {
        None
    };

    let printed_log = tracing_subscriber::fmt::layer().with_filter(
        EnvFilter::builder()
            .with_default_directive("mmserver=info".parse()?)
            .from_env_lossy(),
    );

    tracing_subscriber::registry()
        .with(tracy)
        .with(trace_log)
        .with(printed_log)
        .init();

    Ok(())
}

fn preflight_checks(cfg: &config::Config, vk: &vulkan::VkContext) -> anyhow::Result<()> {
    match linux_version() {
        Some((major, minor)) if major < 6 => {
            bail!("kernel version {major}.{minor} is too low; 6.x required");
        }
        None => warn!("unable to determine linux kernel version!"),
        _ => (),
    }

    match vk.device_info.driver_version {
        vulkan::DriverVersion::MesaRadv {
            major,
            minor,
            patch,
        } => {
            if major < 24 || (major == 24 && minor < 3) {
                bail!("mesa >= 24.3 required, have {major}.{minor}.{patch}");
            }
        }
        vulkan::DriverVersion::NvidiaProprietary { major, minor } => {
            if major < 565 {
                bail!("driver version >= 565.x required, have {major}.{minor}");
            }
        }
        vulkan::DriverVersion::Other(ref driver) => {
            warn!(driver, "using potentially unsupported vulkan driver")
        }
    }

    std::fs::create_dir_all(&cfg.data_home).context(format!(
        "failed to initialize data_home ({})",
        cfg.data_home.display(),
    ))?;

    // Check for Ubuntu's restrictions on rootless containers.
    if sysctl("apparmor_restrict_unprivileged_unconfined")
        || sysctl("apparmor_restrict_unprivileged_userns")
    {
        warn!(
            "Unprivileged user namespaces restricted by AppArmor! Launching applications will \
             fail unless an exception is installed. Read more here: \
             https://ubuntu.com/blog/ubuntu-23-10-restricted-unprivileged-user-namespaces"
        )
    }

    Ok(())
}

fn linux_version() -> Option<(u32, u32)> {
    let uname = rustix::system::uname();
    let version = uname.release().to_str().ok()?;

    let version = version.split_whitespace().next()?;
    let mut parts = version.splitn(3, ".");
    let major = parts.next()?;
    let minor = parts.next()?;

    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn sysctl(name: &str) -> bool {
    const CTL_PATH: &str = "/proc/sys/kernel";

    std::fs::read_to_string(Path::new(CTL_PATH).join(name))
        .map(|s| s.trim() == "1")
        .ok()
        .unwrap_or_default()
}

fn save_vulkaninfo(bug_report_dir: impl AsRef<Path>) {
    match Command::new("vulkaninfo").env_clear().output() {
        Ok(output) => {
            let _ = std::fs::write(
                bug_report_dir.as_ref().join("vulkaninfo.log"),
                output.stdout,
            );
        }
        Err(e) => debug!("failed to run vulkaninfo: {:#}", e),
    }
}

#[test]
fn test_linux_version() {
    let Some((major, _minor)) = linux_version() else {
        panic!("failed to determine linux version");
    };

    assert!(major >= 6);
}
