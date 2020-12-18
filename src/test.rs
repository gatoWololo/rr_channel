/// Utility code for unit testing!
use crate::detthread::{get_det_id, DetThreadId};
use crate::recordlog::{ChannelVariant, RecordEntry, RecordedEvent};
use crate::rr::DetChannelId;
use crate::{InMemoryRecorder, RRMode};
use anyhow::Result;
use std::collections::HashMap;
use std::error::Error;
use std::thread;
use std::time::Duration;

pub(crate) trait ThreadSafe: Send + Sync + 'static {}
impl<T: Send + Sync + 'static> ThreadSafe for T {}

/// Interface shared by channels. Allows our test programs to abstract over channel
/// type.
///
/// Have trait gone too far?
pub(crate) trait TestChannel<T> {
    // Send and 'static needed for channel sender to be moved to different threads.
    type S: Sender<T> + Send + 'static;
    type R;
    fn make_channels(mode: RRMode) -> (Self::S, Self::R);
}

pub(crate) trait Sender<T> {
    type SendError: Error + ThreadSafe;
    fn send(&self, msg: T) -> Result<(), Self::SendError>;
}

pub(crate) trait Receiver<T> {
    type RecvError: Error + ThreadSafe + PartialEq;

    fn recv(&self) -> Result<T, Self::RecvError>;
}

pub(crate) trait TryReceiver<T> {
    type TryRecvError: Error + ThreadSafe + PartialEq;
    fn try_recv(&self) -> Result<T, Self::TryRecvError>;

    const EMPTY: Self::TryRecvError;
    const TRY_DISCONNECTED: Self::TryRecvError;
}

pub(crate) trait ReceiverTimeout<T> {
    type RecvTimeoutError: Error + ThreadSafe + PartialEq;

    fn recv_timeout(&self, timeout: Duration) -> Result<T, Self::RecvTimeoutError>;

    const TIMEOUT: Self::RecvTimeoutError;
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
