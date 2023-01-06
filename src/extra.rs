use std::fmt::{Display};
use std::io::{Write};

use anyhow::{Context as _, Result as AnyResult};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::{MapAccess, Visitor as DeVisitor};

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
pub struct ExtraMap(pub Vec<(String, ExtraValue)>);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExtraValue {
	Int(i64),
	Float(f64),
	String(String),
}

impl ExtraFormat {
	pub fn write<'i>(&self, mut writer: impl Write, pairs: impl IntoIterator<Item = (&'i str, &'i ExtraValue)>) -> AnyResult<()> {
		let mut pairs = pairs.into_iter().peekable();
		if pairs.peek().is_some() {
			match self {
				&ExtraFormat::JsonArray => {
					writer.write(b" ").context("Failed to add pre-JSON space")?;
					serde_json::Serializer::new(writer)
						.collect_seq(pairs.map(|p| p.1))
						.context("Failed to serialize extra data as JSON array")
				},

				&ExtraFormat::JsonMap => {
					writer.write(b" ").context("Failed to add pre-JSON space")?;
					serde_json::Serializer::new(writer)
						.collect_map(pairs)
						.context("Failed to serialize extra data as JSON map")
				},

				&ExtraFormat::SpaceKeyValue => {
					for (k, v) in pairs {
						write!(writer, " {}={}", k, v)?;
					}

					Ok(())
				},

				&ExtraFormat::SpaceValue => {
					for (_, v) in pairs {
						write!(writer, " {}", v)?;
					}

					Ok(())
				},
			}
		} else {
			Ok(())
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
