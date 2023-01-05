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
				let commands = {
					let redeems = config_rx.borrow_and_update();
					let start = redeems.partition_point(|r| *r.name < *redeem.reward_title);

					redeems[start..].iter()
						.take_while(|r| *r.name == *redeem.reward_title)
						.map(|r| r.command.clone())
						.collect::<Vec<_>>()
				};

				let hits = commands.len();
				log::info!(target: "redeem", "{} command{} triggered by redemption of \"{}\" by {} ({}).", hits, if hits != 1 { "s" } else { "" }, redeem.reward_title, redeem.user_name, redeem.user_login);

				for command in commands {
					if rcon_tx.send_async(RconCommand::Handle(command)).await.is_err() {
						log::debug!(target: "redeem", "Stopping redeem processing due to RCON channel closure.");
						break;
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
