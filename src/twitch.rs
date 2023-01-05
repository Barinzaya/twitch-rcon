use std::borrow::{Cow};
use std::ops::{ControlFlow};
use std::sync::{Arc};
use std::time::{Duration};

use anyhow::{Context as _, Error as AnyError, Result as AnyResult, anyhow};
use flume::{Sender as ChannelTx};
use parking_lot::{Mutex};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch::{Receiver as WatchRx};
use tokio_util::sync::{CancellationToken};
use twitch_api2::{TWITCH_PUBSUB_URL};
use twitch_api2::pubsub::{self, Topic, Topics};
use twitch_oauth2::{AccessTokenRef, ValidatedToken};

use crate::config::{TwitchConfig};
use crate::redeem::{Redeem, RedeemCommand};

const BACKOFF_MIN: f64 = 1.0;
const BACKOFF_MAX: f64 = 120.0;
const BACKOFF_GROWTH: f64 = 1.0;
const BACKOFF_JITTER: f64 = 0.1;

const LISTEN_NONCE: &'static str = "Listen";

const PING_JITTER: f64 = 0.1;
const PING_TIME: f64 = 270.0;
const PONG_TIME: f64 = 10.0;

pub async fn run(mut config_rx: WatchRx<Arc<TwitchConfig>>, redeem_tx: ChannelTx<RedeemCommand>, cancel: CancellationToken) {
	while !cancel.is_cancelled() {
		let client_cancel = cancel.child_token();
		let _guard = client_cancel.clone().drop_guard();

		let config = config_rx.borrow_and_update().clone();
		let mut client_task = tokio::spawn(run_client(config, redeem_tx.clone(), client_cancel.clone()));

		tokio::select!{
			biased;
			_ = cancel.cancelled() => break,

			Ok(_) = config_rx.changed() => {
				log::info!(target: "twitch", "Twitch configuration changed. Resetting Twitch client.");
			},

			result = &mut client_task => match result {
				Ok(Ok(())) => {
					log::debug!(target: "twitch", "Twitch client exited cleanly.");
					break
				},

				Ok(Err(e)) => {
					log::error!(target: "twitch", "Twitch client encountered an error: {:#}", e);
					break
				},

				Err(e) => {
					log::error!(target: "twitch", "Twitch client was unable to run: {:#}", e);
					break
				},
			},
		}

		client_cancel.cancel();
		let _ = client_task.await;
	}

	log::info!(target: "twitch", "Twitch interface stopped.");
}

async fn run_client(config: Arc<TwitchConfig>, redeem_tx: ChannelTx<RedeemCommand>, cancel: CancellationToken) -> AnyResult<()> {
	log::debug!(target: "twitch", "Twitch client starting.");
	log::info!(target: "twitch", "Validating authentication token...");

	let channel_token = tokio::select!{
		biased;
		_ = cancel.cancelled() => return Ok(()),
		channel_token = authenticate(&config.auth_token) => channel_token,
	}.context("Failed to validate auth token")?;

	let channel_id = channel_token.user_id.as_ref()
		.context("User token does not specify a user ID!")?
		.as_str()
		.parse::<u32>()
		.context("Failed to parse channel ID")?;

	let channel_login = channel_token.login.as_ref()
		.context("User token does not specify a login!")?;

	if let Some(expires_in) = channel_token.expires_in {
		let expiration = time::OffsetDateTime::now_local()
			.context("Failed to get local time")? + expires_in;
		let expiration_str = expiration.format(&time::format_description::well_known::Rfc2822)
			.context("Failed to format expiration date")?;
		log::info!(target: "twitch", "Authenticated with Twitch as {} ({}). Token expiration: {}", channel_login, channel_id, expiration_str);
	} else {
		log::info!(target: "twitch", "Authenticated with Twitch as {} ({}).", channel_login, channel_id);
	}

	let topics = Arc::from_iter([
		pubsub::channel_points::ChannelPointsChannelV1 { channel_id }.into_topic(),
	]);

	let backoff = Arc::new(Mutex::new(BACKOFF_MIN));

	let result: AnyResult<()> = loop {
		let session_cancel = cancel.child_token();
		let _guard = session_cancel.clone().drop_guard();

		let mut session_task = tokio::spawn(run_session(config.auth_token.clone(), channel_token.clone(), topics.clone(), redeem_tx.clone(), backoff.clone(), session_cancel.clone()));

		let result = tokio::select!{
			biased;
			_ = cancel.cancelled() => ControlFlow::Break(Ok(())),

			result = &mut session_task => match result {
				Ok(Ok(ControlFlow::Continue(()))) => {
					log::debug!(target: "twitch", "Twitch session ended with a clean retry.");
					ControlFlow::Continue(())
				},

				Ok(Err(ControlFlow::Continue(e))) => {
					log::error!(target: "twitch", "Twitch session ended with a recoverable error: {:#}", e);
					ControlFlow::Continue(())
				},

				Ok(Ok(ControlFlow::Break(()))) => {
					log::debug!(target: "twitch", "Twitch session ended with a clean exit.");
					ControlFlow::Break(Ok(()))
				},

				Ok(Err(ControlFlow::Break(e))) => {
					ControlFlow::Break(Err(e)
						.context("Twitch session ended with an unrecoverable error"))
				},

				Err(e) => {
					ControlFlow::Break(Err(e)
						.context("Twitch session failed to run"))
				},
			},
		};

		session_cancel.cancel();
		let _ = session_task.await;

		match result {
			ControlFlow::Break(r) => {
				break r;
			},

			ControlFlow::Continue(()) => {
				let backoff = {
					let mut b = backoff.lock();
					let old = *b;
					*b = f64::clamp(*b + (1.0 + BACKOFF_GROWTH), BACKOFF_MIN, BACKOFF_MAX);
					old
				};

				if backoff > 0.0 {
					let wait = crate::util::jitter(backoff, BACKOFF_JITTER);
					log::info!(target: "twitch", "Attempting to reconnect Twitch session in {:.1} seconds.", wait);
					tokio::time::sleep(Duration::from_secs_f64(wait)).await;
				} else {
					log::info!(target: "twitch", "Attempting to reconnect Twitch session now.");
				}
			},
		}

	};

	log::debug!(target: "twitch", "Twitch client stopped.");
	result
}

async fn authenticate(auth_token: &str) -> AnyResult<Arc<ValidatedToken>> {
	let http_client = reqwest::Client::new();
	let access_token = AccessTokenRef::from_str(&auth_token);

	let user_token = access_token.validate_token(&http_client).await
		.map_err(|e| anyhow!(e))?;
	log::debug!(target: "twitch", "User token: {:?}", user_token);
	Ok(Arc::new(user_token))
}

async fn run_session(auth_token: Arc<str>, channel_token: Arc<ValidatedToken>, topics: Arc<[Topics]>, redeem_tx: ChannelTx<RedeemCommand>, backoff: Arc<Mutex<f64>>, cancel: CancellationToken) -> Result<ControlFlow<()>, ControlFlow<AnyError, AnyError>> {
	log::debug!(target: "twitch", "Twitch session starting.");
	log::info!(target: "twitch", "Connecting to Twitch PubSub API...");

	let listen_cmd = pubsub::listen_command(topics.as_ref(), Some(auth_token.as_ref()), Some(LISTEN_NONCE))
		.context("Failed to construct listen command")
		.with_break_err()?;
	let listen_msg = tungstenite::Message::Text(listen_cmd);

	let mut socket = tokio_tungstenite::connect_async(TWITCH_PUBSUB_URL.as_ref()).await
		.context("Failed to connect to PubSub API")
		.with_continue_err()?
		.0;

	socket.send(listen_msg).await
		.context("Failed to listen for channel point redemptions")
		.with_continue_err()?;

	let mut ping_timer = Box::pin(tokio::time::sleep(Duration::from_secs_f64(crate::util::jitter(PING_TIME, PING_JITTER))));
	let mut pong_timer = Box::pin(tokio::time::sleep(Duration::MAX)); // TODO: A better way to deal with this optional future? Futures-util OptionFutre? Something else?

	let mut result = Ok(ControlFlow::Continue(()));
	let mut stopping = false;

	loop {
		tokio::select!{
			biased;

			_ = cancel.cancelled(), if !stopping => {
				if let Err(e) = socket.close(Some(tungstenite::protocol::frame::CloseFrame {
					code: tungstenite::protocol::frame::coding::CloseCode::Normal,
					reason: Cow::Borrowed("Shutting down"),
				})).await.context("Failed to shut down WebSocket connection") {
					result = result.or(Err(e).with_continue_err());
				}

				stopping = true;
			},

			_ = pong_timer.as_mut(), if !stopping => {
				result = Err(ControlFlow::Continue(anyhow!("Ping was not acknowledged within {:.1} seconds.", PONG_TIME)));
				cancel.cancel();
			},

			_ = ping_timer.as_mut(), if !stopping => {
				let ping_msg = tungstenite::Message::Text(String::from("{\"type\":\"PING\"}"));
				socket.send(ping_msg).await
					.context("Failed to send ping to PubSub interface")
					.with_continue_err()?;
				log::debug!(target: "twitch", "Sent PubSub ping.");

				ping_timer = Box::pin(tokio::time::sleep(Duration::from_secs_f64(crate::util::jitter(PING_TIME, PING_JITTER))));
				pong_timer = Box::pin(tokio::time::sleep(Duration::from_secs_f64(PONG_TIME)));
			},

			msg = socket.next() => match msg {
				None => break,

				Some(Err(e)) => {
					result = Err(e).context("An error occurred reading from the WebSocket").with_continue_err();
					cancel.cancel();
				},

				Some(Ok(tungstenite::Message::Close(_))) => {
					return Ok(ControlFlow::Continue(()));
				},

				Some(Ok(tungstenite::Message::Ping(n))) => {
					socket.send(tungstenite::Message::Pong(n)).await
						.context("Failed to send pong on PubSub socket").with_continue_err()?;
				},

				Some(Ok(tungstenite::Message::Text(s))) => {
					let response = pubsub::Response::parse(&s)
						.context("Failed to parse PubSub message").with_continue_err()?;

					match response {
						pubsub::Response::Message { data: pubsub::TopicData::ChannelPointsChannelV1 { reply, .. }} => {
							match *reply {
								pubsub::channel_points::ChannelPointsChannelV1Reply::RewardRedeemed { redemption, .. } => {
									log::info!(target: "twitch", "{} ({}) redeemed {} ({})", redemption.user.display_name, redemption.user.login, redemption.reward.title, redemption.reward.id);

									redeem_tx.send(RedeemCommand::Handle(Redeem {
										channel_token: channel_token.clone(),

										redeem_id: redemption.id,
										redeem_input: redemption.user_input,

										reward_cost: redemption.reward.cost,
										reward_id: redemption.reward.id,
										reward_title: redemption.reward.title,

										user_id: redemption.user.id,
										user_login: redemption.user.login,
										user_name: redemption.user.display_name,
									})).context("Failed to forward redemption info").with_break_err()?;
								},

								_ => {},
							}
						},

						pubsub::Response::Pong => {
							log::debug!(target: "twitch", "Received PubSub pong.");
							pong_timer = Box::pin(tokio::time::sleep(Duration::MAX));
						},

						pubsub::Response::Reconnect => {
							log::info!(target: "twitch", "Received request to reconnect from Twitch.");
							cancel.cancel();
						},

						pubsub::Response::Response(r) => {
							if r.nonce.as_deref() == Some(LISTEN_NONCE) {
								if let Some(e) = r.error.filter(|e| e.len() > 0) {
									result = Err(anyhow!(e))
										.context("Failed to listen for channel point redemptions")
										.with_break_err();
									cancel.cancel();
								} else {
									log::info!(target: "twitch", "Connected to Twitch and listening for channel point redemptions.");
									*backoff.lock() = 0.0;
								}
							}
						},

						_ => {},
					}
				},

				Some(Ok(m)) => {
					log::debug!(target: "twitch", "Ignoring unsupported data received from PubSub: {:?}", m);
				},
			},
		}
	}

	log::debug!(target: "twitch", "Twitch session stopped.");
	result
}

trait WithControlFlow {
	type WithErr;
	type WithOk;
	fn with_break_err(self) -> Self::WithErr;
	fn with_continue_err(self) -> Self::WithErr;
	fn with_break(self) -> Self::WithOk;
	fn with_continue(self) -> Self::WithOk;
}

impl<T, E> WithControlFlow for Result<T, E>  {
	type WithErr = Result<T, ControlFlow<E, E>>;
	type WithOk = Result<ControlFlow<T, T>, E>;
	fn with_break_err(self) -> Self::WithErr { self.map_err(|e| ControlFlow::Break(e)) }
	fn with_continue_err(self) -> Self::WithErr { self.map_err(|e| ControlFlow::Continue(e)) }
	fn with_break(self) -> Self::WithOk { self.map(|t| ControlFlow::Break(t)) }
	fn with_continue(self) -> Self::WithOk { self.map(|t| ControlFlow::Continue(t)) }
}
