// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod codec;
mod compositor;
mod config;
mod pixel_scale;
mod server;
mod session;
mod state;
mod vulkan;
mod waking_sender;

use std::{
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context, Result};
use clap::Parser;
#[cfg(feature = "ffmpeg_encode")]
use ffmpeg_sys_next as ffmpeg_sys;
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
    /// The path to a config file. By default, /etc/magic-mirror/mmserver.{toml,json} is used (if present).
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

    // Squash ffmpeg logs.
    #[cfg(feature = "ffmpeg_encode")]
    unsafe {
        ffmpeg_sys::av_log_set_level(ffmpeg_sys::AV_LOG_QUIET);
        // TODO: the callback has to be variadic, which means using nightly rust.
        // ffmpeg_sys::av_log_set_callback(Some(ffmpeg_log_callback))
    }

    // Load config.
    let mut cfg = config::Config::new(args.config.as_ref(), &args.include_apps)
        .context("failed to read config")?;

    // Override with command line flags.
    cfg.bug_report_dir = bug_report_dir.clone();
    if let Some(bind) = args.bind {
        cfg.server.bind = bind;
    } else if args.bind_systemd {
        cfg.server.bind_systemd = true;
    }

    let vk = Arc::new(vulkan::VkContext::new(cfg!(debug_assertions))?);

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

    info!("listening on {:?}", srv.local_addr()?);
    srv.run().context("server exited")?;

    if let Some(dir) = &bug_report_dir {
        info!("bug report files saved to: {:?}", dir);
    }

    Ok(())
}

fn init_logging(bug_report_dir: Option<impl AsRef<Path>>) -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;

    let trace_log = if let Some(dir) = bug_report_dir {
        // Additionally write a trace log with everything to the bug report dir.
        let file = std::fs::File::create(dir.as_ref().join("mmserver.log"))?;
        let trace_filter = tracing_subscriber::EnvFilter::new("mmserver=trace");

        let trace_log = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(Mutex::new(file))
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
