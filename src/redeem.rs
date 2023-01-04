use flume::{Receiver as ChannelRx, RecvError as ChannelRxErr, Sender as ChannelTx};
use tokio::sync::watch::{Receiver as WatchRx};

use crate::config::{RedeemConfig};
use crate::rcon::{RconCommand};

#[derive(Clone, Debug)]
pub struct Redeem {
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

pub async fn run(redeem_rx: ChannelRx<RedeemCommand>, mut config_rx: WatchRx<Vec<RedeemConfig>>, rcon_tx: ChannelTx<RconCommand>) {
	loop {
		match redeem_rx.recv_async().await {
			Ok(RedeemCommand::Handle(redeem)) => {
				let command = {
					let redeems = config_rx.borrow_and_update();
					redeems.binary_search_by(|r| r.name.as_ref().cmp(&redeem.reward_title))
						.ok()
						.map(|i| redeems[i].command.clone())
				};

				if let Some(command) = command {
					log::info!(target: "redeem", "Processing \"{}\", redeemed by {} ({}).", redeem.reward_title, redeem.user_name, redeem.user_login);

					if rcon_tx.send_async(RconCommand::Handle(command)).await.is_err() {
						log::debug!(target: "redeem", "Stopping redeem processing due to RCON channel closure.");
						break;
					}
				} else {
					log::info!(target: "redeem", "No action configured for channel point redeem \"{}\".", redeem.reward_title);
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
