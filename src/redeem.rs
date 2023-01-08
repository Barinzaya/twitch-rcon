use std::sync::{Arc};

use flume::{Receiver as ChannelRx, RecvError as ChannelRxErr, Sender as ChannelTx};
use tokio::sync::watch::{Receiver as WatchRx};

use crate::config::{AllRedeemsConfig, RedeemConfig};
use crate::rcon::{RconCommand};

#[derive(Clone, Debug)]
pub struct Redeem {
	pub channel_token: Arc<twitch_oauth2::ValidatedToken>,

	pub redeem_id: twitch_api2::types::RedemptionId,
	pub redeem_input: Option<String>,

	pub reward_cost: u32,
	pub reward_id: twitch_api2::types::RewardId,
	pub reward_title: String,

	pub user_id: twitch_api2::types::UserId,
	pub user_login: twitch_api2::types::Nickname,
	pub user_name: twitch_api2::types::DisplayName,
}

#[derive(Debug)]
pub enum RedeemCommand {
	Handle(Redeem),
}

pub async fn run(redeem_rx: ChannelRx<RedeemCommand>, mut base_config_rx: WatchRx<AllRedeemsConfig>, mut redeems_config_rx: WatchRx<Vec<RedeemConfig>>, rcon_tx: ChannelTx<RconCommand>) {
	loop {
		match redeem_rx.recv_async().await {
			Ok(RedeemCommand::Handle(redeem)) => {
				let channel = redeem.channel_token.login.as_ref()
					.expect("Channel token did not contain a login!");

				let commands = {
                    let base = base_config_rx.borrow_and_update();
					let redeems = redeems_config_rx.borrow_and_update();
					let start = redeems.partition_point(|r| *r.reward < *redeem.reward_title);

					redeems[start..].iter()
						.take_while(|r| *r.reward == *redeem.reward_title)
						.filter(|r| r.channel.as_ref().map(|c| c.as_str() == channel.as_str()).unwrap_or(true))
						.map(|r| r.command(&base).map(Arc::from))
						.collect::<Vec<_>>()
				};

				let hits = commands.len();
				log::info!(target: "redeem", "{} command{} triggered by redemption of \"{}\" by {} ({}) in {}.", hits, if hits != 1 { "s" } else { "" }, redeem.reward_title, redeem.user_name, redeem.user_login, channel);

				for command in commands {
					match command {
						Ok(command) => {
							log::debug!(target: "redeem", "Requesting command to be sent: {}", command);

							if rcon_tx.send_async(RconCommand::Handle(command)).await.is_err() {
								log::debug!(target: "redeem", "Stopping redeem processing due to RCON channel closure.");
								break;
							}
						},

						Err(e) => log::error!(target: "redeem", "Failed to generate command to send: {:#}", e),
					}
				}
			},

			Err(ChannelRxErr::Disconnected) => {
				log::debug!(target: "redeem", "Stopping redeem processing due to redeem channel closure.");
				break;
			},
		}
	}

	log::info!(target: "redeem", "Redeem processing stopped.");
}
