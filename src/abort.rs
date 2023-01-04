use tokio_util::sync::{CancellationToken};

pub async fn run(cancel: CancellationToken) {
    tokio::select!{
        _ = cancel.cancelled() => {},
        r = tokio::signal::ctrl_c() => match r {
            Ok(()) => {
                log::info!(target: "abort", "Ctrl+C was pressed! Shutting down...");
                cancel.cancel();
            },

            Err(e) => {
                log::error!(target: "abort", "Failed to wait for Ctrl+C: {e}");
            }
        },
    }
}
