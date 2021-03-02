//! Crossbeam, ipc, and mpsc channels share a lot in common. This module provides traits and methods
//! to abstract over some of their differences. This allows to us tests these different channel
//! implementations using the tests without worrying about what exact channel we're testing.

use crate::detthread::{get_det_id, DetThreadId};
use crate::recordlog::{ChannelVariant, RecordEntry, RecordedEvent};
use crate::rr::DetChannelId;
use crate::{InMemoryRecorder, RRMode};
use anyhow::Result;
use std::collections::HashMap;
use std::error::Error;
use std::thread;
use std::time::Duration;

/// Our shorthand trait (aka trait alias) representing trait safety. Necessary as our channels will
/// be sent across different threads.
/// TODO: Switch to nightly and use trait_alias feature?
pub(crate) trait ThreadSafe: Send + Sync + 'static {}
impl<T: Send + Sync + 'static> ThreadSafe for T {}

/// A generic representation of a channel pair. Includes the types for the Sending and Receiving end
/// as well as a method to create these channels.
pub(crate) trait TestChannel<T> {
    // Send and 'static needed for channel sender to be moved to different threads.
    /// `S` Represents the concrete type of the Sender end of the channel. Must implement _our_
    /// test::Sender trait below.
    type S: Sender<T> + Send + 'static;
    /// `R` Represents the concrete type of the Receiver end of the channel. Must implement _our_
    /// test::Receiver trait below.
    type R;
    /// Create a pair of connected channels with the given RRMode.
    fn make_channels(mode: RRMode) -> (Self::S, Self::R);
}

/// Representation of the sending end of our channels.
pub(crate) trait Sender<T> {
    /// Represents the  error type returned on sending failure.
    type SendError: Error + ThreadSafe;
    fn send(&self, msg: T) -> Result<(), Self::SendError>;
}

/// Representation of the receiving end of our channels.
pub(crate) trait Receiver<T> {
    /// Represents error type returned on receiving failure.
    type RecvError: Error + ThreadSafe + PartialEq;

    fn recv(&self) -> Result<T, Self::RecvError>;
}

/// Represents the `try_recv` method for channels.
pub(crate) trait TryReceiver<T> {
    /// Represents the error type returned on receiving failure.
    type TryRecvError: Error + ThreadSafe + PartialEq;
    fn try_recv(&self) -> Result<T, Self::TryRecvError>;

    /// This const is the value returned when a `try_recv` happens on an empty channel.
    const EMPTY: Self::TryRecvError;
    /// This const is the value returned when a `try_recv` happens on a disconnected channel.
    const TRY_DISCONNECTED: Self::TryRecvError;
}

/// Represents the `recv_timeout` method for channels.
pub(crate) trait ReceiverTimeout<T> {
    type RecvTimeoutError: Error + ThreadSafe + PartialEq;

    fn recv_timeout(&self, timeout: Duration) -> Result<T, Self::RecvTimeoutError>;

    /// This const is the value returned when a `recv_timeout` times out.
    const TIMEOUT: Self::RecvTimeoutError;
    /// This const is the value returned when a `recv_timeout` happens on a disconnected channel.
    const DISCONNECTED: Self::RecvTimeoutError;
}

pub(crate) fn simple_program<C: TestChannel<i32>>(r: RRMode) -> Result<()>
where
    C::R: Receiver<i32>,
{
    let (s, r) = C::make_channels(r);
    s.send(1)?;
    s.send(3)?;
    s.send(5)?;
    r.recv()?;
    r.recv()?;
    r.recv()?;
    Ok(())
}

pub(crate) fn simple_program_manual_log(
    send_event: RecordedEvent,
    recv_event: impl Fn(DetThreadId) -> RecordedEvent,
    variant: ChannelVariant,
) -> HashMap<DetThreadId, InMemoryRecorder> {
    let dti = get_det_id();
    let channel_id = DetChannelId::from_raw(dti.clone(), 1);

    let tn = std::any::type_name::<i32>();
    let re = RecordEntry::new(send_event, variant, channel_id.clone(), tn.to_string());

    let mut rf: InMemoryRecorder = InMemoryRecorder::new(dti.clone());
    rf.add_entry(re.clone());
    rf.add_entry(re.clone());
    rf.add_entry(re);

    let re = RecordEntry::new(recv_event(dti.clone()), variant, channel_id, tn.to_string());
    rf.add_entry(re.clone());
    rf.add_entry(re.clone());
    rf.add_entry(re);

    let mut hm = HashMap::new();
    hm.insert(dti, rf);
    hm
}

pub(crate) fn recv_program<C: TestChannel<i32>>(r: RRMode) -> Result<()>
where
    C::R: Receiver<i32>,
{
    let (s, r) = C::make_channels(r);
    let _ = s.send(1)?;
    let _ = s.send(2)?;

    assert_eq!(r.recv(), Ok(1));
    assert_eq!(r.recv(), Ok(2));
    Ok(())
}

pub(crate) fn try_recv_program<C: TestChannel<i32>>(r: RRMode) -> Result<()>
where
    C::R: TryReceiver<i32>,
{
    let (s, r) = C::make_channels(r);
    assert_eq!(r.try_recv(), Err(C::R::EMPTY));

    s.send(5)?;
    drop(s);

    assert_eq!(r.try_recv(), Ok(5));
    assert_eq!(r.try_recv(), Err(C::R::TRY_DISCONNECTED));
    Ok(())
}

pub(crate) fn recv_timeout_program<C: TestChannel<i32>>(r: RRMode) -> Result<()>
where
    C::R: ReceiverTimeout<i32>,
{
    let (s, r) = C::make_channels(r);

    let h = crate::detthread::spawn::<_, Result<()>>(move || {
        thread::sleep(Duration::from_millis(10));
        s.send(5)?;
        drop(s);
        Ok(())
    });

    assert_eq!(
        r.recv_timeout(Duration::from_micros(10)),
        Err(C::R::TIMEOUT)
    );

    assert_eq!(r.recv_timeout(Duration::from_millis(40)), Ok(5));
    assert_eq!(
        r.recv_timeout(Duration::from_millis(50)),
        Err(C::R::DISCONNECTED)
    );

    // "Unlike with normal errors, this value doesn't implement the Error trait." So we unwrap
    // instead. Not sure why std::thread::Result doesn't impl Result...
    h.join().unwrap()?;
    Ok(())
}
