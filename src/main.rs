mod abort;
mod config;
mod extra;
mod rcon;
mod redeem;
mod twitch;
mod util;

use std::process::{ExitCode};
use std::sync::{Arc};

use anyhow::{Context as _, Result as AnyResult};
use tokio_util::sync::{CancellationToken};

use crate::config::{AppConfig};

fn main() -> ExitCode {
    init_logger().expect("Failed to set up logger");
    log::info!("Twitch to RCON bot starting...");

    match run_sync() {
        Ok(()) => {
            log::info!("Closing log file (clean exit).");
            log::logger().flush();
            ExitCode::SUCCESS
        },

        Err(e) => {
            log::error!("An unhandled error has occurred: {:#}", e);
            log::info!("Closing log file (crash exit).");
            log::logger().flush();

            println!("Press any key to exit.");
            let _ = console::Term::stdout().read_key();
            ExitCode::FAILURE
        }
    }
}

fn init_logger() -> AnyResult<()> {
    let term_config = simplelog::ConfigBuilder::new()
        .set_time_offset_to_local().unwrap_or_else(|e| e)
        .build();

    let term_logger = simplelog::TermLogger::new(
        log::LevelFilter::Debug,
        term_config,
        simplelog::TerminalMode::Mixed,
        simplelog::ColorChoice::Auto);

    let file = std::fs::File::options()
        .append(true)
        .create(true)
        .open("twitch-rcon.log")
        .context("Failed to open log file")?;

    let file_config = simplelog::ConfigBuilder::new()
        .set_time_format_custom(simplelog::format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"))
        .set_time_offset_to_local().unwrap_or_else(|e| e)
        .build();

    let file_logger = simplelog::WriteLogger::new(
        log::LevelFilter::Trace,
        file_config,
        file);

    simplelog::CombinedLogger::init(vec![term_logger, file_logger])
        .context("Failed to install logger")
}

#[cfg(feature = "multi-thread")]
fn run_sync() -> AnyResult<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create multi-threaded Tokio runtime")?
        .block_on(run_async())
}

#[cfg(not(feature = "multi-thread"))]
fn run_sync() -> AnyResult<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Failed to create Tokio runtime")?
        .block_on(run_async())
}

async fn run_async() -> AnyResult<()> {
    let cancel = CancellationToken::new();
    let mut set = tokio::task::JoinSet::new();

    {
        set.spawn(abort::run(cancel.clone()));

        let config_path = tokio::fs::canonicalize("twitch-rcon.toml").await
            .context("Failed to canonicalize config path")?;
        let config = AppConfig::read_from(&config_path).await
            .context("Failed to load application config")?;

        let (config_tx, config_rx) = flume::bounded(1);
        let (rcon_config_tx, rcon_config_rx) = tokio::sync::watch::channel(Arc::new(config.rcon));
        let (redeem_config_tx, redeem_config_rx) = tokio::sync::watch::channel(config.redeems);
        let (twitch_config_tx, twitch_config_rx) = tokio::sync::watch::channel(Arc::new(config.twitch));

        set.spawn(AppConfig::watch(config_path, config_tx, cancel.clone()));
        set.spawn(config::distribute(config_rx, rcon_config_tx, redeem_config_tx, twitch_config_tx));

        let (rcon_tx, rcon_rx) = flume::bounded(1);
        let (redeem_tx, redeem_rx) = flume::bounded(1);

        set.spawn(rcon::run(rcon_rx, rcon_config_rx));
        set.spawn(redeem::run(redeem_rx, redeem_config_rx, rcon_tx));
        set.spawn(twitch::run(twitch_config_rx, redeem_tx, cancel.clone()));
    }

    while let Some(result) = set.join_next().await {
        match result {
            Ok(()) => {},
            Err(e) => {
                cancel.cancel();
                log::error!("Service failed to run: {:#}", e);
            },
        }
    }

    Ok(())
}
