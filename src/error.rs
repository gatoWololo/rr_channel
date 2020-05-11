//! Error Module

use crate::detthread::DetThreadId;
use crate::recordlog::{ChannelLabel, EventId, RecordedEvent};
use crate::rr::DetChannelId;
use crossbeam_channel::{RecvError, RecvTimeoutError, TryRecvError};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;

/// Fundamentally just wanna know if it was a Timeout or Disconnected error. This type is
/// used to abstract over different types of RecvErrors from different channel types.
#[derive(Debug)]
pub enum RecvErrorRR {
    Timeout,
    Disconnected,
}

impl From<RecvTimeoutError> for RecvErrorRR  {
    fn from(e: RecvTimeoutError) -> RecvErrorRR {
        match e {
            RecvTimeoutError::Timeout => RecvErrorRR::Timeout,
            RecvTimeoutError::Disconnected => RecvErrorRR::Disconnected,
        }
    }
}

impl From<RecvErrorRR> for DesyncError {
    fn from(e: RecvErrorRR) -> DesyncError {
        match e {
            RecvErrorRR::Disconnected => DesyncError::Disconnected,
            RecvErrorRR::Timeout => DesyncError::Timedout,
        }
    }
}

/// Desynchonizations are common (TODO: make desync less common). This error type
/// enumerates all reasons why we might desynchronize when executing RR.
#[derive(Debug)]
pub enum DesyncError {
    /// Missing entry for specified (DetThreadId, EventIt) pair.
    NoEntryInLog(DetThreadId, EventId),
    /// ChannelVariants don't match. log flavor, vs current flavor.
    ChannelVariantMismatch(ChannelLabel, ChannelLabel),
    /// Channel ids don't match. log channel, vs current flavor.
    ChannelMismatch(DetChannelId, DetChannelId),
    /// RecordedEvent log message was different. logged event vs current event.
    EventMismatch(RecordedEvent, RecordedEvent),
    /// This thread did not get its DetThreadId initialized even though it should
    /// have.
    UnitializedDetThreadId,
    /// Receiver was expected for select operation but entry is missing.
    MissingReceiver(u64),
    /// An entry already exists for the specificied index in IpcReceiverSet.
    SelectExistingEntry(u64),
    /// IpcReceiverSet expected first index, but found second index.
    SelectIndexMismatch(u64, u64),
    /// Generic error used for situations where we already know we're
    /// desynchronized and want to alert caller.
    Desynchronized,
    /// Expected a channel close message, but saw actual value.
    ChannelClosedExpected,
    /// Receiver disconnected...
    Disconnected,
    /// Expected a message, but channel returned closed.
    ChannelClosedUnexpected(u64),
    /// Waited too long and no message ever came.
    Timedout,
    /// Thread woke up after assuming it had ran of the end of the log.
    /// While `NoEntryInLog` may mean that this thread simply blocked forever
    /// or "didn't make it this far", DesynchronizedWakeup means we put that
    /// thread to sleep and it was woken up by a desync somewhere.
    DesynchronizedWakeup,
}

// We want to Serialize Types defined in other crates. So we copy their definition here.
// (This is how serde wants us to do it)

#[derive(Serialize, Deserialize)]
#[serde(remote = "RecvTimeoutError")]
pub enum RecvTimeoutErrorDef {
    Timeout,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "TryRecvError")]
pub enum TryRecvErrorDef {
    Empty,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "RecvError")]
pub struct RecvErrorDef {}

#[derive(Serialize, Deserialize)]
#[serde(remote = "mpsc::TryRecvError")]
pub enum MpscTryRecvErrorDef {
    Empty,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "mpsc::RecvTimeoutError")]
pub enum MpscRecvTimeoutErrorDef {
    Timeout,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "mpsc::RecvError")]
pub struct MpscRecvErrorDef {}
