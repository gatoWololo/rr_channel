use log::Level::*;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::crossbeam_channel::{Receiver, Sender};
use crate::detthread;
use crate::ipc_channel::ipc::{
    self, IpcReceiver, IpcReceiverSet, IpcSelectionResult, IpcSender, OpaqueIpcMessage,
    OpaqueIpcReceiver,
};
use crate::DetMessage;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::thread::JoinHandle;

lazy_static! {
    pub static ref ROUTER: RouterProxy = RouterProxy::new();
}

/// OMAR: Must wrap to use our channel type.
pub struct RouterProxy {
    comm: Mutex<RouterProxyComm>,
}

impl RouterProxy {
    pub fn new() -> RouterProxy {
        let (msg_sender, msg_receiver) = crate::crossbeam_channel::unbounded();
        let (wakeup_sender, wakeup_receiver) = ipc::channel().unwrap();

        let handle =
            crate::detthread::spawn(move || Router::new(msg_receiver, wakeup_receiver).run());
        RouterProxy {
            comm: Mutex::new(RouterProxyComm {
                msg_sender,
                wakeup_sender,
                shutdown: false,
                handle: Some(handle),
            }),
        }
    }

    /// Send a shutdown message to the router containing a ACK sender,
    /// send a wakeup message to the router, and block on the ACK.
    /// Calling it is idempotent,
    /// which can be useful when running a multi-process system in single-process mode.
    pub fn shutdown(&self) -> std::thread::Result<()> {
        let mut comm = self.comm.lock().unwrap();

        if comm.shutdown {
            return Ok(());
        }
        comm.shutdown = true;

        let (ack_sender, ack_receiver) = crate::crossbeam_channel::unbounded();
        let _ = comm
            .wakeup_sender
            .send(())
            .map(|_| {
                comm.msg_sender
                    .send(RouterMsg::Shutdown(ack_sender))
                    .unwrap();
                ack_receiver.recv().expect("No ack message received.");
                // ack_receiver.recv().unwrap();
            })
            .unwrap();

        let h = comm
            .handle
            .take()
            .expect("This should never happen! JoinHandle already taken.");
        h.join()
    }

    // This is somewhere where our tivo wrapper diverges from the official ipc-channel API.
    // Original:
    // pub fn add_route(&self, receiver: OpaqueIpcReceiver, callback: RouterHandler)
    // For some reason they ask for an OpaqueIpcReceiver. We instead ask for an IpcReceiver<T>
    // and internally call `.to_opaque()`. We do this because we need the type information
    // internally.
    pub fn add_route<T: 'static>(
        &self,
        receiver: IpcReceiver<T>,
        mut callback: Box<dyn FnMut(Result<T, ipc_channel::Error>) + Send>,
    ) where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        let comm = self.comm.lock().unwrap();

        let callback_wrapper = Box::new(move |msg: OpaqueIpcMessage| {
            // We want to forward the DetThreadId. Access the real opaque channel
            // underneath our wrapper. As our wrapper throws the DetThreadId away.
            match msg.opaque.to::<DetMessage<T>>() {
                Ok((forward_id, msg)) => {
                    // Big Hack: Temporarily set TLS DetThreadId so original sender's
                    // DetThreadId is properly forwarded to receiver.
                    let original_id = detthread::get_det_id();

                    detthread::start_forwarding_id(forward_id);
                    callback(Ok(msg));
                    detthread::stop_forwarding_id(original_id);
                }
                Err(e) => {
                    callback(Err(e));
                }
            }
        });

        comm.msg_sender
            .send(RouterMsg::AddRoute(receiver.to_opaque(), callback_wrapper))
            .unwrap();

        comm.wakeup_sender.send(()).unwrap();
    }

    /// A convenience function to route an `IpcReceiver<T>` to an existing `Sender<T>`.
    pub fn route_ipc_receiver_to_crossbeam_sender<T>(
        &self,
        ipc_receiver: IpcReceiver<T>,
        crossbeam_sender: Sender<T>,
    ) where
        T: for<'de> Deserialize<'de> + Serialize + Send + 'static,
    {
        crate::log_rr!(
            Info,
            "Routing IpcReceiver<{:?}> to crossbeam_sender: {:?}",
            ipc_receiver.metadata.id,
            crossbeam_sender.metadata.id
        );
        self.add_route(
            ipc_receiver,
            Box::new(move |message| drop(crossbeam_sender.send(message.unwrap()))),
        )
    }

    /// A convenience function to route an `IpcReceiver<T>` to a `Receiver<T>`: the most common
    /// use of a `Router`.
    pub fn route_ipc_receiver_to_new_crossbeam_receiver<T>(
        &self,
        ipc_receiver: IpcReceiver<T>,
    ) -> Receiver<T>
    where
        T: for<'de> Deserialize<'de> + Serialize + Send + 'static,
    {
        crate::log_rr!(Info, "Routing IpcReceiver<{:?}>", ipc_receiver.metadata.id);

        let (crossbeam_sender, crossbeam_receiver) = crate::crossbeam_channel::unbounded();
        crate::log_rr!(
            Info,
            "Created Channels<{:?} for routing.",
            crossbeam_receiver.metadata.id
        );

        self.route_ipc_receiver_to_crossbeam_sender(ipc_receiver, crossbeam_sender);
        crossbeam_receiver
    }
}

impl Default for RouterProxy {
    fn default() -> Self {
        RouterProxy::new()
    }
}

/// OMAR: Must wrap since we need it to use our rr channels.
struct RouterProxyComm {
    msg_sender: Sender<RouterMsg>,
    wakeup_sender: IpcSender<()>,
    shutdown: bool,
    /// The router spawns a thread when executed. If something goes wrong in this thread, it
    /// silently panics as no one is checking its return. This is not great for CI unit testing.
    /// Instead when shutting down the thread ROUTER, via `.shutdown()` we return our handle.
    handle: Option<JoinHandle<()>>,
}

struct Router {
    msg_receiver: Receiver<RouterMsg>,
    msg_wakeup_id: u64,
    ipc_receiver_set: IpcReceiverSet,
    handlers: HashMap<u64, RouterHandler>,
}

/// OMAR: Must wrap since it is not public.
impl Router {
    fn new(msg_receiver: Receiver<RouterMsg>, wakeup_receiver: IpcReceiver<()>) -> Router {
        let mut ipc_receiver_set = IpcReceiverSet::new().unwrap();
        let msg_wakeup_id = ipc_receiver_set.add(wakeup_receiver).unwrap();
        Router {
            msg_receiver,
            msg_wakeup_id,
            ipc_receiver_set,
            handlers: HashMap::new(),
        }
    }

    fn run(&mut self) {
        while let Ok(results) = self.ipc_receiver_set.select() {
            for result in results.into_iter() {
                match result {
                    IpcSelectionResult::MessageReceived(id, _) if id == self.msg_wakeup_id => {
                        match self
                            .msg_receiver
                            .recv()
                            .expect("rr_channel:: RouterProxy::run(): Unable to receive message.")
                        {
                            RouterMsg::AddRoute(receiver, handler) => {
                                let new_receiver_id =
                                    self.ipc_receiver_set.add_opaque(receiver).expect(
                                        "rr_channel:: RouterProxy::run(): Could not add_opaque",
                                    );
                                self.handlers.insert(new_receiver_id, handler);
                                // println!("Added receiver {:?} at {:?} for handler", id, new_receiver_id);
                            }
                            RouterMsg::Shutdown(sender) => {
                                sender
                                    .send(())
                                    .expect("Failed to send confirmation of shutdown.");
                                return;
                            }
                        }
                    }
                    IpcSelectionResult::MessageReceived(id, message) => {
                        let handler = self.handlers.get_mut(&id).
                            unwrap_or_else(|| panic!("rr_channel:: RouterProxy::run(): MessageReceived, No such handler: {:?}", id));
                        handler(message)
                    }
                    IpcSelectionResult::ChannelClosed(id) => {
                        let _handler = self.handlers.remove(&id).
                            unwrap_or_else(|| panic!("rr_channel:: RouterProxy::run(): Channel Closed, No such handler: {:?}", id));
                    }
                }
            }
        }
    }
}

enum RouterMsg {
    AddRoute(OpaqueIpcReceiver, RouterHandler),
    /// Shutdown the router, providing a sender to send an acknowledgement.
    Shutdown(Sender<()>),
}

pub type RouterHandler = Box<dyn FnMut(OpaqueIpcMessage) + Send>;

/// These tests should not access the ROUTER directly. As this leads to weird issues as it persists
/// between the record execution and replay execution in our tests below. Instead, use RouterProxy.
#[cfg(test)]
mod tests {
    use crate::detthread::DetThreadId;
    use crate::ipc_channel::ipc;
    use crate::ipc_channel::router::RouterProxy;
    use crate::recordlog::{ChannelVariant, RecordEntry, RecordedEvent};
    use crate::rr::DetChannelId;
    use crate::test::{rr_test, set_rr_mode};
    use crate::{
        detthread, init_tivo_thread_root, set_global_memory_recorder, InMemoryRecorder, RRMode,
    };
    use anyhow::Result;
    use rusty_fork::rusty_fork_test;
    use std::collections::HashMap;

    /// Doesn't actually test much, but exercises the common code paths for the router.
    /// This will always be deterministic as there is no multiple producers.
    fn add_route() -> Result<()> {
        let (sender, receiver) = ipc::channel::<i32>()?;
        let (sender2, receiver2) = ipc::channel::<i32>()?;
        let router = RouterProxy::new();

        let f = Box::new(move |result: Result<i32, _>| {
            sender2
                .send(result.unwrap())
                .expect("Unable to send message.")
        });

        router.add_route::<i32>(receiver, f);

        for i in 0..20 {
            sender.send(i)?;
        }

        for _ in 0..20 {
            receiver2
                .recv()
                .expect("Cannot recv messages after router.");
        }

        router.shutdown().expect("Router thread errored.");
        Ok(())
    }

    /// Use multiple producers on separate threads to generate data! Checks that the values are the
    /// same.
    fn add_route_mpsc() -> Result<Vec<(i32, i32)>> {
        // Sends (thread #, value)
        let (sender, receiver) = ipc::channel::<(i32, i32)>()?;
        let (in_router_sender, from_router_receiver) = ipc::channel::<(i32, i32)>()?;

        let thread_sender1 = sender.clone();
        let thread_sender2 = sender;

        let router = RouterProxy::new();
        const ITERS: i32 = 1_000;
        let f =
            Box::new(move |result: Result<_, _>| in_router_sender.send(result.unwrap()).unwrap());

        router.add_route(receiver, f);
        let h1 = detthread::spawn::<_, Result<()>>(move || {
            for i in 0..ITERS {
                thread_sender1.send((1, i))?;
            }
            Ok(())
        });
        let h2 = detthread::spawn::<_, Result<()>>(move || {
            for j in 0..ITERS {
                thread_sender2.send((2, j))?;
            }
            Ok(())
        });

        let mut v = Vec::with_capacity((ITERS * 2) as usize);
        for _ in 0..ITERS * 2 {
            v.push(from_router_receiver.recv().unwrap());
        }

        h1.join().unwrap()?;
        h2.join().unwrap()?;

        router.shutdown().unwrap();

        Ok(v)
    }

    /// Does pretty much the same as `add_route_mpsc` but uses the provided method, so it is
    /// shorter!
    fn route_ipc_receiver_to_new_crossbeam_receiver_mpsc(iters: i32) -> Result<()> {
        let router = RouterProxy::new();
        let (s, r) = ipc::channel::<(i32, i32)>()?;

        let cr = router.route_ipc_receiver_to_new_crossbeam_receiver(r);
        let thread_sender1 = s.clone();
        let thread_sender2 = s;

        let h1 = detthread::spawn::<_, Result<()>>(move || {
            for i in 0..iters {
                thread_sender1.send((1, i))?;
            }
            Ok(())
        });

        let h2 = detthread::spawn::<_, Result<()>>(move || {
            for j in 0..iters {
                thread_sender2.send((2, j))?;
            }
            Ok(())
        });

        let mut v = Vec::with_capacity((iters * 2) as usize);
        for _ in 0..iters * 2 {
            v.push(cr.recv().unwrap());
        }

        h1.join().unwrap()?;
        h2.join().unwrap()?;
        router.shutdown().unwrap();
        Ok(())
    }

    /// Does replay using our manually recordlog. Basically all messages arrive from T2 before any
    /// message arrives from T1. In practice this would be astronomically unlikely but we can force
    /// rr-channels to verify this ordering.
    fn router_manual_recordlog() -> Result<()> {
        env_logger::init();
        init_tivo_thread_root();
        let iters = 5_000;

        // The [..] is needed as there is no as_slice method for arrays? Weird...
        // Main thread
        let main_dti = DetThreadId::from(&[][..]);
        // Router Thread
        let router_dti = DetThreadId::from(&[1][..]);
        // First sender thread
        let dti1 = DetThreadId::from(&[2][..]);
        // Second sender thread
        let dti2 = DetThreadId::from(&[3][..]);

        let tn = std::any::type_name::<i32>().to_string();

        // Channel we use to send new messages to the router. Used by T1 and T2.
        let threads_sender = DetChannelId::from_raw(main_dti.clone(), 1);
        // Channel passed to router to recv messages from.
        let router_receiver = DetChannelId::from_raw(main_dti.clone(), 1);
        // Channel created by router to send messages into in function
        // `route_ipc_receiver_to_new_crossbeam_receiver`.
        let router_sender = DetChannelId::from_raw(main_dti.clone(), 2);
        // Channel returned by router's `route_ipc_receiver_to_new_crossbeam_receiver` method.
        // Used by main thread to read messages.
        let main_recv = DetChannelId::from_raw(main_dti.clone(), 2);

        let mut imr1 = InMemoryRecorder::new(dti1.clone());
        let mut imr2 = InMemoryRecorder::new(dti2.clone());
        let mut main_thread_imr = InMemoryRecorder::new(main_dti.clone());
        let mut router_thread_imr = InMemoryRecorder::new(router_dti.clone());

        let thread_sender_record = RecordEntry::new(
            RecordedEvent::IpcSender,
            ChannelVariant::Ipc,
            threads_sender.clone(),
            tn.clone(),
        );

        let main_record = RecordEntry::new(
            RecordedEvent::CbRecvSucc {
                sender_thread: router_dti.clone(),
            },
            ChannelVariant::CbUnbounded,
            main_recv,
            tn.clone(),
        );

        // Router receiving messages from T1.
        let router_record_recv_t1 = RecordEntry::new(
            RecordedEvent::IpcRecvSucc {
                sender_thread: dti1.clone(),
            },
            ChannelVariant::Ipc,
            router_receiver.clone(),
            tn.clone(),
        );
        // Router receiving messages from T2.
        let router_record_recv_t2 = RecordEntry::new(
            RecordedEvent::IpcRecvSucc {
                sender_thread: dti2.clone(),
            },
            ChannelVariant::Ipc,
            router_receiver.clone(),
            tn.clone(),
        );

        let router_record_send = RecordEntry::new(
            RecordedEvent::CbSender,
            ChannelVariant::CbUnbounded,
            router_sender,
            tn.clone(),
        );

        for _ in 0..iters {
            imr1.add_entry(thread_sender_record.clone());
            imr2.add_entry(thread_sender_record.clone());
            main_thread_imr.add_entry(main_record.clone());
            // Pretend we saw all messages from T2 first.
            router_thread_imr.add_entry(router_record_recv_t2.clone());
            router_thread_imr.add_entry(router_record_send.clone());
        }

        // Now pretend we saw all the entries for T1
        for _ in 0..iters {
            router_thread_imr.add_entry(router_record_recv_t1.clone());
            router_thread_imr.add_entry(router_record_send.clone());
        }

        let mut manual_recordlog: HashMap<DetThreadId, InMemoryRecorder> = HashMap::new();
        manual_recordlog.insert(dti1, imr1);
        manual_recordlog.insert(dti2, imr2);
        manual_recordlog.insert(main_dti, main_thread_imr);
        manual_recordlog.insert(router_dti, router_thread_imr);

        set_global_memory_recorder(manual_recordlog);

        set_rr_mode(RRMode::Replay);
        route_ipc_receiver_to_new_crossbeam_receiver_mpsc(iters)?;
        Ok(())
    }

    rusty_fork_test! {
    #[test]
    fn add_route_test() -> Result<()> {
        rr_test(add_route)
    }

    #[test]
    fn add_route_mpsc_test() -> Result<()> {
        rr_test(add_route_mpsc)
    }

    #[test]
    fn route_ipc_receiver_to_new_crossbeam_receiver_mpsc_test() -> Result<()> {
        rr_test(|| route_ipc_receiver_to_new_crossbeam_receiver_mpsc(10_000))
    }

    // #[test]
    // fn router_manual_recordlog_test() -> Result<()> {
    //     router_manual_recordlog()
    // }
    }
}
