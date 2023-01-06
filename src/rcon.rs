use std::{sync::{Arc}, time::Duration};

use anyhow::{Context as _, Result as AnyResult};
use flume::{Receiver as ChannelRx, RecvError as ChannelRxErr, Sender as ChannelTx};
use tokio::sync::watch::{Receiver as WatchRx};
use tokio_util::sync::{CancellationToken};

use crate::config::{RconConfig, SetRconMode};

const BACKOFF_MIN: f64 = 1.0;
const BACKOFF_MAX: f64 = 30.0;
const BACKOFF_GROWTH: f64 = 0.5;
const BACKOFF_JITTER: f64 = 0.1;

#[derive(Debug)]
pub enum RconCommand {
	Handle(Arc<str>),
}

pub async fn run(rcon_rx: ChannelRx<RconCommand>, mut config_rx: WatchRx<Arc<RconConfig>>, cancel: CancellationToken) {
	let (send_tx, send_rx) = flume::bounded(1);
	let (resend_tx, resend_rx) = flume::bounded(1);

	let command_task = tokio::spawn(async move {
		while let Ok(command) = rcon_rx.recv_async().await {
			match command {
				RconCommand::Handle(command) => {
					match send_tx.send_async(command).await {
						Ok(()) => {},

						Err(_) => {
							log::debug!(target: "rcon", "Stopping RCON command task due to closure of command outtake channel.");
							return;
						},
					}
				},
			}
		}

		log::debug!(target: "rcon", "Stopping RCON command task due to closure of command intake channel.");
	});

	while !cancel.is_cancelled() {
		let config = config_rx.borrow_and_update().clone();
		let client_cancel = cancel.child_token();
		let mut client_task = tokio::spawn(run_client(config, send_rx.clone(), resend_rx.clone(), resend_tx.clone(), client_cancel.clone()));
		let mut client_result = None;

		tokio::select!{
			biased;

			result = &mut client_task => {
				client_result = Some(result);
			},

			Ok(_) = config_rx.changed() => {
				log::info!(target: "rcon", "RCON configuration changed! Resetting RCON client.");
			},
		}

		let client_result = if let Some(r) = client_result {
			r
		} else {
			client_cancel.cancel();
			client_task.await
		};

		match client_result {
			Ok(()) => log::debug!(target: "rcon", "RCON main task exited cleanly."),
			Err(e) => {
				log::warn!(target: "rcon", "RCON client task encountered an error! Resetting RCON client.");
				log::error!(target: "rcon", "RCON client error: {:#}", e);
			},
		}
	}

	drop(send_rx);
	match command_task.await {
		Ok(()) => log::debug!(target: "rcon", "RCON command task exited cleanly."),
		Err(e) => log::error!(target: "rcon", "RCON command task encountered an error: {:#}", e),
	}

	log::info!(target: "rcon", "RCON client stopped.");
}

async fn run_client(config: Arc<RconConfig>, send_rx: ChannelRx<Arc<str>>, resend_rx: ChannelRx<Arc<str>>, resend_tx: ChannelTx<Arc<str>>, cancel: CancellationToken) {
	let mut backoff = BACKOFF_MIN;

	while !send_rx.is_disconnected() {
		let result = async {
			let mut conn = rcon::Connection::builder()
				.set_mode(config.mode)
				.connect(&config.host, &config.password)
				.await
				.context("Failed to connect to RCON server")?;
			log::info!(target: "rcon", "Connected to RCON server at {}.", config.host);

			backoff = 0.0;

			loop {
				let recv = tokio::select!{
					biased;
					recv = resend_rx.recv_async() => recv,
					recv = send_rx.recv_async() => recv,
					_ = cancel.cancelled() => {
						log::debug!(target: "rcon", "RCON client task stopping due to cancellation.");
						break
					},
				};

				let command = match recv {
					Ok(command) => {
						crate::util::DropSend::new(command, config.retry.then_some(&resend_tx))
					},

					Err(ChannelRxErr::Disconnected) => {
						log::debug!(target: "rcon", "Stopping RCON client task due to closure of command send channel.");
						break
					},
				};

				log::info!(target: "rcon", "Sending RCON command: {}", command.as_ref());
				let response = conn.cmd(command.as_ref()).await
					.context("Failed to send RCON command")?;
				let response = response.trim();

				command.consume();

				if !response.is_empty() {
					log::info!(target: "rcon", "Received RCON response: {}", response);
				}
			}

			AnyResult::<()>::Ok(())
		}.await;

		match result {
			Ok(()) => {
				log::debug!(target: "rcon", "RCON client task stopped.");
				break
			},

			Err(e) => {
				log::error!(target: "rcon", "RCOM client has encountered an error: {:#}", e);

				if cancel.is_cancelled() {
					log::info!(target: "rcon", "RCON client stopping now.");
					break;
				}

				if backoff > 0.0 {
					let wait = crate::util::jitter(backoff, BACKOFF_JITTER);
					log::info!(target: "rcon", "RCON client will reconnect in {:.1} second(s).", wait);

					tokio::select!{
						biased;

						_ = cancel.cancelled() => {
							log::info!(target: "rcon", "RCON reconnect cancelled due to shutdown.");
							break;
						},

						_ = tokio::time::sleep(Duration::from_secs_f64(wait)) => {},
					}
				} else {
					log::info!(target: "rcon", "RCON client reconnecting now.");
				}

				backoff = f64::clamp(backoff * (1.0 + BACKOFF_GROWTH), BACKOFF_MIN, BACKOFF_MAX);
			}
		}
	}
}
