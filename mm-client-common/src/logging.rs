// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::sync::{Arc, OnceLock};

#[derive(uniffi::Enum)]
pub enum LogLevel {
    None,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<log::Level> for LogLevel {
    fn from(value: log::Level) -> Self {
        match value {
            log::Level::Trace => LogLevel::Trace,
            log::Level::Debug => LogLevel::Debug,
            log::Level::Info => LogLevel::Info,
            log::Level::Warn => LogLevel::Warn,
            log::Level::Error => LogLevel::Error,
        }
    }
}

/// An interface for receiving logs from this library.
#[uniffi::export(with_foreign)]
pub trait LogDelegate: Send + Sync + std::fmt::Debug {
    fn log(&self, level: LogLevel, target: String, msg: String);
}

struct LogWrapper(Arc<dyn LogDelegate>);

impl log::Log for LogWrapper {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            LogDelegate::log(
                &*self.0,
                record.level().into(),
                record.target().to_owned(),
                record.args().to_string(),
            )
        }
    }

    fn flush(&self) {}
}

/// Set the minimum log level.
#[uniffi::export]
fn set_log_level(level: LogLevel) {
    let filter = match level {
        LogLevel::None => log::LevelFilter::Off,
        LogLevel::Trace => log::LevelFilter::Trace,
        LogLevel::Debug => log::LevelFilter::Debug,
        LogLevel::Info => log::LevelFilter::Info,
        LogLevel::Warn => log::LevelFilter::Warn,
        LogLevel::Error => log::LevelFilter::Error,
    };

    log::set_max_level(filter);
}

/// Set the global logger.
#[uniffi::export]
fn set_logger(logger: Arc<dyn LogDelegate>) {
    // This has to accept an Arc to be exportable by uniffi, however awkward
    // that may be.
    static LOGGER: OnceLock<LogWrapper> = OnceLock::new();

    let logger = LOGGER.get_or_init(|| LogWrapper(logger));
    log::set_logger(logger).expect("failed to set logger")
}
