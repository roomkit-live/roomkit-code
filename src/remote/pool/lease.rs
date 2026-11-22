//! Keep a claimed Redis slot until the turn finishes or ownership cannot be confirmed.
use super::{queue::Ticket, registry::Registry};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_util::sync::CancellationToken;

// Redis uses whole seconds for its 20-second lease. Measure from before each
// acknowledged request so the owner stops relying on a slot before Redis expires it.
const CONFIRMED_FOR: Duration = Duration::from_secs(18);
const INTERVAL: Duration = Duration::from_secs(3);
const RETRY: Duration = Duration::from_secs(1);

pub(super) struct Lease {
    pub stop: CancellationToken,
    pub lost: CancellationToken,
}

impl Lease {
    pub fn start(
        reg: Registry,
        instance: &str,
        turn: String,
        claimed: Instant,
        ticket: Arc<Ticket>,
    ) -> Self {
        let stop = CancellationToken::new();
        let lost = CancellationToken::new();
        let done = stop.clone();
        let expired = lost.clone();
        let instance = instance.to_owned();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                () = done.cancelled() => {}
                () = maintain(claimed, || reg.renew(&instance, &turn)) => expired.cancel(),
            }
            drop(ticket);
            let _ = timeout(INTERVAL, reg.release(&instance, &turn)).await;
        });
        Self { stop, lost }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// An explicit refusal ends ownership. A transport error can recover within
/// the last confirmed lease; only the idempotent renewal is retried.
async fn maintain<F: Future<Output = Result<bool, String>>>(
    mut confirmed: Instant,
    mut renew: impl FnMut() -> F,
) {
    let mut delay = INTERVAL;
    loop {
        let deadline = confirmed + CONFIRMED_FOR;
        if timeout_at(deadline, sleep(delay)).await.is_err() || Instant::now() >= deadline {
            return;
        }
        let sent = Instant::now();
        let response = timeout_at(deadline.min(sent + INTERVAL), renew()).await;
        match response {
            Ok(Ok(true)) if Instant::now() < deadline => {
                confirmed = sent;
                delay = INTERVAL;
            }
            Ok(Ok(_)) => return,
            _ => delay = RETRY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, future::ready};

    #[tokio::test(start_paused = true)]
    async fn transient_errors_retry_only_until_renewal_or_expiry() {
        let calls = Cell::new(0);
        let start = Instant::now();
        maintain(start, || {
            calls.set(calls.get() + 1);
            ready(match calls.get() {
                1 => Err("offline".into()),
                2 => Ok(true),
                _ => Ok(false),
            })
        })
        .await;
        assert_eq!(calls.get(), 3);
        assert_eq!(start.elapsed(), INTERVAL * 2 + RETRY);
        let last = Instant::now();
        maintain(last, std::future::pending).await;
        assert_eq!(last.elapsed(), CONFIRMED_FOR);
    }
}
