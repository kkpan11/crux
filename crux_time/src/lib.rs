//! Current time access for Crux apps
//!
//! Current time (on a wall clock) is considered a side-effect (although if we were to get pedantic, it's
//! more of a side-cause) by Crux, and has to be obtained externally. This capability provides a simple
//! interface to do so.

pub mod command;
pub mod duration;
pub mod error;
pub mod instant;

pub use duration::Duration;
pub use error::TimeError;
pub use instant::Instant;

use serde::{Deserialize, Serialize};

use crux_core::capability::{CapabilityContext, Operation};
use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{LazyLock, Mutex},
    task::Poll,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimeRequest {
    Now,
    NotifyAt { id: TimerId, instant: Instant },
    NotifyAfter { id: TimerId, duration: Duration },
    Clear { id: TimerId },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimerId(pub usize);

fn get_timer_id() -> TimerId {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    TimerId(COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimeResponse {
    Now { instant: Instant },
    InstantArrived { id: TimerId },
    DurationElapsed { id: TimerId },
    Cleared { id: TimerId },
}

impl Operation for TimeRequest {
    type Output = TimeResponse;
}

/// The Time capability API
///
/// This capability provides access to the current time and allows the app to ask for
/// notifications when a specific instant has arrived or a duration has elapsed.
pub struct Time<Ev> {
    context: CapabilityContext<TimeRequest, Ev>,
}

impl<Ev> crux_core::Capability<Ev> for Time<Ev> {
    type Operation = TimeRequest;
    type MappedSelf<MappedEv> = Time<MappedEv>;

    fn map_event<F, NewEv>(&self, f: F) -> Self::MappedSelf<NewEv>
    where
        F: Fn(NewEv) -> Ev + Send + Sync + 'static,
        Ev: 'static,
        NewEv: 'static + Send,
    {
        Time::new(self.context.map_event(f))
    }

    #[cfg(feature = "typegen")]
    fn register_types(generator: &mut crux_core::typegen::TypeGen) -> crux_core::typegen::Result {
        generator.register_type::<Instant>()?;
        generator.register_type::<Duration>()?;
        generator.register_type::<Self::Operation>()?;
        generator.register_type::<<Self::Operation as Operation>::Output>()?;
        Ok(())
    }
}

impl<Ev> Clone for Time<Ev> {
    fn clone(&self) -> Self {
        Self {
            context: self.context.clone(),
        }
    }
}

impl<Ev> Time<Ev>
where
    Ev: 'static,
{
    pub fn new(context: CapabilityContext<TimeRequest, Ev>) -> Self {
        Self { context }
    }

    /// Request current time, which will be passed to the app as a [`TimeResponse`] containing an [`Instant`]
    /// wrapped in the event produced by the `callback`.
    pub fn now<F>(&self, callback: F)
    where
        F: FnOnce(TimeResponse) -> Ev + Send + Sync + 'static,
    {
        self.context.spawn({
            let context = self.context.clone();
            let this = self.clone();

            async move {
                context.update_app(callback(this.now_async().await));
            }
        });
    }

    /// Request current time, which will be passed to the app as a [`TimeResponse`] containing an [`Instant`]
    /// This is an async call to use with [`crux_core::compose::Compose`].
    pub async fn now_async(&self) -> TimeResponse {
        self.context.request_from_shell(TimeRequest::Now).await
    }

    /// Ask to receive a notification when the specified [`Instant`] has arrived.
    pub fn notify_at<F>(&self, instant: Instant, callback: F) -> TimerId
    where
        F: FnOnce(TimeResponse) -> Ev + Send + Sync + 'static,
    {
        let (future, id) = self.notify_at_async(instant);
        self.context.spawn({
            let context = self.context.clone();
            async move {
                context.update_app(callback(future.await));
            }
        });
        id
    }

    /// Ask to receive a notification when the specified [`Instant`] has arrived.
    /// This is an async call to use with [`crux_core::compose::Compose`].
    pub fn notify_at_async(
        &self,
        instant: Instant,
    ) -> (TimerFuture<impl Future<Output = TimeResponse>>, TimerId) {
        let id = get_timer_id();
        let future = self
            .context
            .request_from_shell(TimeRequest::NotifyAt { id, instant });
        (TimerFuture::new(id, future), id)
    }

    /// Ask to receive a notification when the specified duration has elapsed.
    pub fn notify_after<F>(&self, duration: Duration, callback: F) -> TimerId
    where
        F: FnOnce(TimeResponse) -> Ev + Send + Sync + 'static,
    {
        let (future, id) = self.notify_after_async(duration);
        self.context.spawn({
            let context = self.context.clone();
            async move {
                context.update_app(callback(future.await));
            }
        });
        id
    }

    /// Ask to receive a notification when the specified duration has elapsed.
    /// This is an async call to use with [`crux_core::compose::Compose`].
    pub fn notify_after_async(
        &self,
        duration: Duration,
    ) -> (TimerFuture<impl Future<Output = TimeResponse>>, TimerId) {
        let id = get_timer_id();
        let future = self
            .context
            .request_from_shell(TimeRequest::NotifyAfter { id, duration });
        (TimerFuture::new(id, future), id)
    }

    pub fn clear(&self, id: TimerId) {
        self.context.spawn({
            {
                let mut lock = CLEARED_TIMER_IDS.lock().unwrap();
                lock.insert(id);
            }

            let context = self.context.clone();
            async move {
                context.notify_shell(TimeRequest::Clear { id }).await;
            }
        });
    }
}

pub struct TimerFuture<F>
where
    F: Future<Output = TimeResponse> + Unpin,
{
    timer_id: TimerId,
    is_cleared: bool,
    future: F,
}

impl<F> Future for TimerFuture<F>
where
    F: Future<Output = TimeResponse> + Unpin,
{
    type Output = TimeResponse;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.is_cleared {
            // short-circuit return
            return Poll::Ready(TimeResponse::Cleared { id: self.timer_id });
        };
        // see if the timer has been cleared
        let timer_is_cleared = {
            let mut lock = CLEARED_TIMER_IDS.lock().unwrap();
            lock.remove(&self.timer_id)
        };
        let this = self.get_mut();
        this.is_cleared = timer_is_cleared;
        if timer_is_cleared {
            // if the timer has been cleared, immediately return 'Ready' without
            // waiting for the timer to elapse
            Poll::Ready(TimeResponse::Cleared { id: this.timer_id })
        } else {
            // otherwise, defer to the inner future
            Pin::new(&mut this.future).poll(cx)
        }
    }
}

impl<F> TimerFuture<F>
where
    F: Future<Output = TimeResponse> + Unpin,
{
    fn new(timer_id: TimerId, future: F) -> Self {
        Self {
            timer_id,
            future,
            is_cleared: false,
        }
    }
}

// Global HashSet containing the ids of timers which have been _cleared_
// but the whose futures have _not since been polled_. When the future is next
// polled, the timer id is evicted from this set and the timer is 'poisoned'
// so as to return immediately without waiting on the shell.
static CLEARED_TIMER_IDS: LazyLock<Mutex<HashSet<TimerId>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_serializing_the_request_types_as_json() {
        let now = TimeRequest::Now;

        let serialized = serde_json::to_string(&now).unwrap();
        assert_eq!(&serialized, "\"now\"");

        let deserialized: TimeRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(now, deserialized);

        let now = TimeRequest::NotifyAt {
            id: TimerId(1),
            instant: Instant::new(1, 2).expect("valid instant"),
        };

        let serialized = serde_json::to_string(&now).unwrap();
        assert_eq!(
            &serialized,
            r#"{"notifyAt":{"id":1,"instant":{"seconds":1,"nanos":2}}}"#
        );

        let deserialized: TimeRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(now, deserialized);

        let now = TimeRequest::NotifyAfter {
            id: TimerId(2),
            duration: Duration::from_secs(1).expect("valid duration"),
        };

        let serialized = serde_json::to_string(&now).unwrap();
        assert_eq!(
            &serialized,
            r#"{"notifyAfter":{"id":2,"duration":{"nanos":1000000000}}}"#
        );

        let deserialized: TimeRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(now, deserialized);
    }

    #[test]
    fn test_serializing_the_response_types_as_json() {
        let now = TimeResponse::Now {
            instant: Instant::new(1, 2).expect("valid instant"),
        };

        let serialized = serde_json::to_string(&now).unwrap();
        assert_eq!(
            &serialized,
            r#"{"now":{"instant":{"seconds":1,"nanos":2}}}"#
        );

        let deserialized: TimeResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(now, deserialized);

        let now = TimeResponse::DurationElapsed { id: TimerId(1) };

        let serialized = serde_json::to_string(&now).unwrap();
        assert_eq!(&serialized, r#"{"durationElapsed":{"id":1}}"#);

        let deserialized: TimeResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(now, deserialized);

        let now = TimeResponse::InstantArrived { id: TimerId(2) };

        let serialized = serde_json::to_string(&now).unwrap();
        assert_eq!(&serialized, r#"{"instantArrived":{"id":2}}"#);

        let deserialized: TimeResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(now, deserialized);
    }
}
