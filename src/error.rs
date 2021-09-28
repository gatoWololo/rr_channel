//! Error Module

use crate::recordlog::{ChannelVariant, TivoEvent};
use crate::rr::DetChannelId;
use crossbeam_channel::{RecvError, RecvTimeoutError, TryRecvError};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use thiserror::Error;

/// Fundamentally just wanna know if it was a Timeout or Disconnected error. This type is
/// used to abstract over different types of RecvErrors from different channel types.
#[derive(Debug)]
pub enum RecvErrorRR {
    Timeout,
    Disconnected,
}

impl From<RecvTimeoutError> for RecvErrorRR {
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
            RecvErrorRR::Timeout => DesyncError::Timeout(None),
        }
    }
}

/// Desynchonizations are common (TODO: make desync less common). This error type
/// enumerates all reasons why we might desynchronize when executing RR.
#[derive(Error, Debug)]
pub(crate) enum DesyncError {
    /// Missing entry for specified (DetThreadId, EventIt) pair.
    /// TODO: Should probably carry information about which entry we're missing.
    #[error("Ran off the end of the log.")]
    NoEntryInLog,
    /// ChannelVariants don't match. log flavor, vs current flavor.
    #[error("Mismatched channel variants. Log showed {0:?} but actual was {1:?}")]
    ChannelVariantMismatch(ChannelVariant, ChannelVariant),
    /// Channel ids don't match. log channel, vs current flavor.
    #[error("Mismatched channel IDs. Log showed {0} but actual was {1}")]
    ChannelMismatch(DetChannelId, DetChannelId),
    /// TivoEvent log message was different. logged event vs current event.
    #[error("Mismatched events. Log showed {0:?} but actual current event was {1:?}.")]
    EventMismatch(TivoEvent, TivoEvent),
    // /// This thread did not get its DetThreadId initialized even though it should
    // /// have.
    // #[error("Uninitialized Thread")]
    // UnitializedDetThreadId,
    /// Receiver was expected for select operation but entry is missing.
    #[error("Missing receiver in Select Set. At index {0:?}")]
    MissingReceiver(u64),
    /// An entry already exists for the specified index in IpcReceiverSet.
    #[error("IpcSelect Entry already exists at {0:?}")]
    SelectExistingEntry(u64),
    /// Generic error used for situations where we already know we're
    /// desynchronized and want to alert caller.
    #[error("RR Channels are desynchronized.")]
    Desynchronized,
    /// Expected a channel close message, but saw actual value.
    #[error("Expected a channel close message, but saw actual value.")]
    ChannelClosedExpected,
    /// Receiver disconnected...
    #[error("Receiver unexpectedly disconnected.")]
    Disconnected,
    /// Expected a message, but channel returned closed.
    #[error("IpcReceiverSet expected a message, but channel returned closed at index {0}")]
    ChannelClosedUnexpected(u64),
    /// Waited too long and no message ever came.
    #[error("Timed out! Waiting for message of type: {0:?}")]
    Timeout(Option<String>),
    /// Thread woke up after assuming it had ran of the end of the log.
    /// While `NoEntryInLog` may mean that this thread simply blocked forever
    /// or "didn't make it this far", DesynchronizedWakeup means we put that
    /// thread to sleep and it was woken up by a desync somewhere.
    #[error("Thread woke up after assuming it had ran of the end of the log.")]
    DesynchronizedWakeup,
  
    /// Unable to write event to log.
    ///1. execution dome has been called 2. TIVO execution object has been dropped early
    //#[error("Cannot write event to recordlog. Reason {0}")]
    #[error("Execution Thread tried to wakeup after it has been dropped.")]
    //CannotWriteEventToLog(#[source] crossbeam_channel::SendError<RecordEntry>),
    CannotWriteEventToLog,
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
