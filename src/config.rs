use std::fmt::{Display};
use std::io::{Write};
use std::path::{Path};
use std::sync::{Arc};
use std::time::{Duration};

use anyhow::{Context as _, Error as AnyError, Result as AnyResult, ensure};
use flume::{Receiver as ChannelRx, Sender as ChannelTx};
use notify::{Watcher as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::{MapAccess, Visitor as DeVisitor};
use tokio::sync::watch::{Sender as WatchTx};
use tokio_util::sync::{CancellationToken};

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
	pub rcon: RconConfig,
	pub twitch: TwitchConfig,

	#[serde(default = "Vec::new")]
	pub redeems: Vec<RedeemConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct RconConfig {
	pub host: String,
	pub password: String,
	pub mode: RconMode,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum RconMode {
	Factorio,
	Minecraft,
	Source,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct RedeemConfig {
	pub reward: String,
	pub channel: Option<String>,
	pub command: String,

	#[serde(flatten)]
	pub extra: ExtraMap,

	#[serde(default)]
	pub format: ExtraFormat,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ExtraFormat {
	JsonArray,
	#[default]
	JsonMap,

	SpaceKeyValue,
	SpaceValue,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExtraMap(Vec<(String, ExtraValue)>);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExtraValue {
	Int(i64),
	Float(f64),
	String(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TwitchConfig {
	pub auth_token: Arc<str>,
}

impl AppConfig {
	pub async fn read_from(path: impl AsRef<Path>) -> AnyResult<Self> {
		let path = path.as_ref();
		let raw_config = tokio::fs::read(path).await
			.with_context(|| format!("Failed to read config file <{}>", path.display()))?;

		let mut config = toml_edit::de::from_slice::<Self>(&raw_config)
			.with_context(|| format!("Failed to parse config file <{}>", path.display()))?;

        // TODO: Technically, this should be done in a blocking context, but as there should only
        // ever be a few redeems, it's probably not worth doing so
        config.redeems.sort_by(|a, b| a.reward.cmp(&b.reward));

		config.validate()?;
		Ok(config)
	}

	pub fn validate(&self) -> AnyResult<()> {
		self.rcon.validate().context("Failed to validate RCON configuration")?;
		self.twitch.validate().context("Failed to validate Twitch configuration")?;

		for redeem in self.redeems.iter() {
			redeem.validate().context("Failed to validate redeem configuration")?;
		}

		Ok(())
	}

	pub async fn watch(path: impl AsRef<Path>, config_tx: ChannelTx<AppConfig>, cancel: CancellationToken) {
		let path = path.as_ref();
		let result: Result<(), AnyError> = async move {
			let (change_tx, change_rx) = flume::bounded(1);

			let mut watcher = notify::recommended_watcher(move |e| { let _ = change_tx.send(e); })
				.context("Failed to create config change watcher")?;
			watcher.watch(path, notify::RecursiveMode::NonRecursive)
				.context("Failed to watch config file")?;

			let mut load_timer = tokio::time::sleep(Duration::MAX);

			loop {
				tokio::select!{
					biased;

					_ = cancel.cancelled() => break Ok(()),

					r = change_rx.recv_async() => {
						let _ = r
							.context("Failed to communicate config change events")?
							.context("Failed to listen for config change events")?;

						load_timer = tokio::time::sleep(Duration::from_millis(100));
					},

					_ = load_timer => {
						log::info!(target: "config", "Configuration file change detected! Loading updated config.");
						load_timer = tokio::time::sleep(Duration::MAX);

						match AppConfig::read_from(path).await {
							Ok(c) => config_tx.send_async(c).await.context("Failed to propagate updated configuration")?,
							Err(e) => log::warn!(target: "config", "Failed to load updated config: {:#}", e),
						}
					},
				}
			}
		}.await;

		if let Err(e) = result {
			log::warn!(target: "config", "Failed to watch for config changes: {:#}", e);
			log::warn!(target: "config", "Config changes will not be detected!");
		}
	}
}

impl RconConfig {
    pub fn validate(&self) -> AnyResult<()> {
        ensure!(self.host.len() > 0, "An RCON host must be specified. See twitch-rcon.toml for more information.");
		Ok(())
    }
}

impl RconMode {
    pub fn apply_to<T>(&self, builder: rcon::Builder<T>) -> rcon::Builder<T> {
		builder
			.enable_factorio_quirks(self == &RconMode::Factorio)
			.enable_minecraft_quirks(self == &RconMode::Minecraft)
    }
}

impl RedeemConfig {
	pub fn command(&self) -> AnyResult<String> {
		let mut result = self.command.clone().into_bytes();

		self.extra.write_as(&mut result, self.format)
			.context("Failed to serialize extra data for command")?;

		let result = String::from_utf8(result)
			.context("Failed to validate command as UTF-8 data")?;
		Ok(result)
	}

    pub fn validate(&self) -> AnyResult<()> {
        ensure!(self.reward.len() > 0, "A channel point reward name must be configured for every redeem.");
        ensure!(self.command.len() > 0, "A command to send must be configured for every redeem.");

		Ok(())
    }
}

impl TwitchConfig {
    pub fn validate(&self) -> AnyResult<()> {
        ensure!(self.auth_token.len() > 0, "A Twitch auth token must be configured. See twitch-rcon.toml for more information.");
		Ok(())
    }
}

pub trait SetRconMode {
	fn set_mode(self, mode: RconMode) -> Self;
}

impl<T> SetRconMode for rcon::Builder<T> {
	fn set_mode(self, mode: RconMode) -> Self {
		mode.apply_to(self)
	}
}

pub async fn distribute(config_rx: ChannelRx<AppConfig>, rcon_tx: WatchTx<Arc<RconConfig>>, redeem_tx: WatchTx<Vec<RedeemConfig>>, twitch_tx: WatchTx<Arc<TwitchConfig>>) {
    while let Ok(AppConfig { rcon, redeems, twitch, .. }) = config_rx.recv_async().await {
        rcon_tx.send_if_modified(move |old| {
            let changed = old.as_ref() != &rcon;
            if changed {
                log::info!(target: "config", "RCON configuration change detected!");
                *old = Arc::new(rcon);
            }
            changed
        });

        redeem_tx.send_if_modified(move |old| {
            let changed = old != &redeems;
            if changed {
                log::info!(target: "config", "Redeem configuration change detected!");
                *old = redeems;
            }
            changed
        });

        twitch_tx.send_if_modified(move |old| {
            let changed = old.as_ref() != &twitch;
            if changed {
                log::info!(target: "config", "Twitch configuration change detected!");
                *old = Arc::new(twitch);
            }
            changed
        });

    }
}

impl ExtraMap {
	pub fn write_as<W: Write>(&self, mut writer: W, format: ExtraFormat) -> AnyResult<()> {
		if self.0.is_empty() {
			return Ok(());
		}

		match format {
			ExtraFormat::JsonArray => {
				writer.write(b" ").context("Failed to add pre-JSON space")?;
				serde_json::Serializer::new(writer)
					.collect_seq(self.0.iter().map(|&(_, ref v)| v))
					.context("Failed to serialize extra data as JSOn array")
			},

			ExtraFormat::JsonMap => {
				writer.write(b" ").context("Failed to add pre-JSON space")?;
				serde_json::Serializer::new(writer)
					.collect_map(self.0.iter().map(|&(ref k, ref v)| (k, v)))
					.context("Failed to serialize extra data as JSOn map")
			},

			ExtraFormat::SpaceKeyValue => {
				for (k, v) in &self.0 {
					writer.write_fmt(format_args!(" {}={}", k, v))
						.context("Failed to format extra data as space-key-value")?;
				}

				Ok(())
			},

			ExtraFormat::SpaceValue => {
				for (_, v) in &self.0 {
					writer.write_fmt(format_args!(" {}", v))
						.context("Failed to format extra data as space-value")?;
				}

				Ok(())
			},
		}
	}
}

impl Display for ExtraValue {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			&ExtraValue::Float(x) => write!(f, "{}", x),
			&ExtraValue::Int(x) => write!(f, "{}", x),
			&ExtraValue::String(ref x) => write!(f, "{}", x),
		}
	}
}

impl<'d> Deserialize<'d> for ExtraMap {
	fn deserialize<D: Deserializer<'d>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_map(ExtraMapDeVisitor(ExtraMap(Vec::new())))
	}
}

impl Serialize for ExtraMap {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_map(self.0.iter().map(|&(ref k, ref v)| (k, v)))
	}
}

struct ExtraMapDeVisitor(ExtraMap);

impl<'d> DeVisitor<'d> for ExtraMapDeVisitor {
	type Value = ExtraMap;

	fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
		formatter.write_str("a string-value mapping")
	}

	fn visit_map<M: MapAccess<'d>>(mut self, mut map: M) -> Result<Self::Value, M::Error> {
		if let Some(hint) = map.size_hint() {
			self.0.0.reserve(hint);
		}

		while let Some((k, v)) = map.next_entry()? {
			self.0.0.push((k, v));
		}

		Ok(self.0)
	}
}
