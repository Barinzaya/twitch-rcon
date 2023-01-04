use std::ops::{Deref, DerefMut};

use flume::{Sender as ChannelTx};

pub fn jitter(nominal: f64, jitter: f64) -> f64 {
	nominal * (1.0 - jitter * (1.0 - 2.0 * fastrand::f64()))
}

pub struct DropSend<'c, T: 'c>(Option<T>, &'c ChannelTx<T>);

impl<'c, T: 'c> DropSend<'c, T> {
	pub fn new(message: T, channel: &'c ChannelTx<T>) -> Self {
		DropSend(Some(message), channel)
	}

	pub fn consume(mut self) -> T {
		self.0.take().unwrap()
	}
}

impl<'c, T: 'c> AsMut<T> for DropSend<'c, T> {
	fn as_mut(&mut self) -> &mut T {
		self.0.as_mut().unwrap()
	}
}

impl<'c, T: 'c> AsRef<T> for DropSend<'c, T> {
	fn as_ref(&self) -> &T {
		self.0.as_ref().unwrap()
	}
}

impl<'c, T: 'c> Deref for DropSend<'c, T> {
	type Target = T;
	fn deref(&self) -> &Self::Target {
		self.as_ref()
	}
}

impl<'c, T: 'c> DerefMut for DropSend<'c, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.as_mut()
	}
}

impl<'c, T: 'c> Drop for DropSend<'c, T> {
	fn drop(&mut self) {
		if let Some(message) = self.0.take() {
			self.1.send(message).expect("Failed to send on drop.");
		}
	}
}
