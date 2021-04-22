use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::JoinHandle;

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

use crate::crossbeam_channel::{Receiver, Sender};
use crate::detthread::get_det_id;
use crate::fn_basename;
use crate::ipc_channel::ipc::{
    self, IpcReceiver, IpcReceiverSet, IpcSelectionResult, IpcSender, OpaqueIpcMessage,
    OpaqueIpcReceiver,
};
use crate::DetMessage;

lazy_static! {
    pub static ref ROUTER: RouterProxy = RouterProxy::new();
}

/// OMAR: Must wrap to use our channel type.
pub struct RouterProxy {
    comm: Mutex<RouterProxyComm>,
}

impl RouterProxy {
    /// We want to initialize the lazy static associated with this. So call its empty method.
    pub fn init(&self) {}

    pub fn new() -> RouterProxy {
        let _s = span!(Level::INFO, "ROUTER Init", init_thread = ?get_det_id()).entered();
        let (msg_sender, msg_receiver) = crate::crossbeam_channel::unbounded();
        let (wakeup_sender, wakeup_receiver) = ipc::channel().unwrap();

        let handle = crate::detthread::spawn(move || {
            let _span = span!(Level::INFO, "ROUTER").entered();
            Router::new(msg_receiver, wakeup_receiver).run()
        });
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
        let _s = self.span(fn_basename!());
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
        let _s = self.span(fn_basename!());
        let comm = self.comm.lock().unwrap();

        let callback_wrapper = Box::new(move |msg: OpaqueIpcMessage| {
            // We want to forward the DetThreadId. Access the real opaque channel
            // underneath our wrapper. As our wrapper throws the DetThreadId away.
            match msg.opaque.to::<DetMessage<T>>() {
                Ok((_, msg)) => {
                    callback(Ok(msg));
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

    fn span(&self, fn_name: &str) -> EnteredSpan {
        span!(Level::INFO, stringify!(RouterProxy), fn_name).entered()
    }

    /// A convenience function to route an `IpcReceiver<T>` to an existing `Sender<T>`.
    pub fn route_ipc_receiver_to_crossbeam_sender<T>(
        &self,
        ipc_receiver: IpcReceiver<T>,
        crossbeam_sender: Sender<T>,
    ) where
        T: for<'de> Deserialize<'de> + Serialize + Send + 'static,
    {
        let _e = self.span(fn_basename!());
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
        let _e = self.span(fn_basename!());

        let (crossbeam_sender, crossbeam_receiver) = crate::crossbeam_channel::unbounded();
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
    use std::collections::HashMap;
    use std::collections::VecDeque;

    use anyhow::Result;
    use rusty_fork::rusty_fork_test;

    use crate::detthread::DetThreadId;
    use crate::ipc_channel::ipc;
    use crate::ipc_channel::router::RouterProxy;

    use crate::recordlog::{
        set_global_memory_replayer, ChannelVariant, IpcSelectEvent, RecordEntry, TivoEvent,
    };
    use crate::rr::DetChannelId;
    use crate::test::{rr_test, set_rr_mode};
    use crate::{detthread, RRMode, Tivo};

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
    fn route_ipc_receiver_to_new_crossbeam_receiver_mpsc(iters: i32) -> Result<Vec<(i32, i32)>> {
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

        Ok(v)
    }

    /// Does replay using our manually recordlog. Basically all messages arrive from T2 before any
    /// message arrives from T1. In practice this would be astronomically unlikely but we can force
    /// rr-channels to verify this ordering.
    fn get_manual_recordlog(iters: i32) -> HashMap<DetThreadId, VecDeque<RecordEntry>> {
        // The [..] is needed as there is no as_slice method for arrays? Weird...
        // Main thread
        let main_thread = DetThreadId::from(&[][..]);
        // Router Thread
        let router_thread = DetThreadId::from(&[1][..]);
        // First sender thread
        let thread1 = DetThreadId::from(&[2][..]);
        // Second sender thread
        let thread2 = DetThreadId::from(&[3][..]);

        let tn = std::any::type_name::<i32>().to_string();

        // Channel passed to router to recv messages from.
        let router_main_interface = DetChannelId::from_raw(main_thread.clone(), 1);
        // Internal router channel.
        let internal_router_channels = DetChannelId::from_raw(main_thread.clone(), 2);
        // Channel we use to send new messages to the router. Used by T1 and T2.
        let threads_sender = DetChannelId::from_raw(main_thread.clone(), 3);
        // Channel returned by router's `route_ipc_receiver_to_new_crossbeam_receiver` method.
        // Used by main thread to read messages.
        let main_recv = DetChannelId::from_raw(main_thread.clone(), 4);
        let shutdown_ack = DetChannelId::from_raw(main_thread.clone(), 5);

        let mut imr1 = VecDeque::new();
        let mut imr2 = VecDeque::new();
        let mut main_thread_imr = VecDeque::new();
        let mut router_thread_imr = VecDeque::new();

        // Router sends messages when routes are added to it!
        let cb_sender = RecordEntry::new(
            TivoEvent::CrossbeamSender,
            ChannelVariant::CbUnbounded,
            router_main_interface.clone(),
            tn.clone(),
        );

        main_thread_imr.push_back(RecordEntry::new(
            TivoEvent::ThreadInitialized(router_thread.clone()),
            ChannelVariant::None,
            DetChannelId::fake(),
            "Thread".to_string(),
        ));

        main_thread_imr.push_back(cb_sender.clone());

        // Router sends messages when routes are added to it!
        main_thread_imr.push_back(RecordEntry::new(
            TivoEvent::IpcSender,
            ChannelVariant::Ipc,
            internal_router_channels.clone(),
            tn.clone(),
        ));

        // Init Router's internal IpcSelector.
        router_thread_imr.push_back(RecordEntry::new(
            TivoEvent::IpcSelectAdd(0),
            ChannelVariant::Ipc,
            internal_router_channels.clone(),
            tn.clone(),
        ));

        let select = RecordEntry::new(
            TivoEvent::IpcSelect {
                select_events: vec![IpcSelectEvent::MessageReceived(0, main_thread.clone())],
            },
            ChannelVariant::Ipc,
            router_main_interface.clone(),
            tn.clone(),
        );
        router_thread_imr.push_back(select.clone());

        router_thread_imr.push_back(RecordEntry::new(
            TivoEvent::CrossbeamRecvSucc {
                sender_thread: main_thread.clone(),
            },
            ChannelVariant::CbUnbounded,
            router_main_interface.clone(),
            tn.clone(),
        ));

        // New route added to router, added to IpcSelect.
        router_thread_imr.push_back(RecordEntry::new(
            TivoEvent::IpcSelectAdd(1),
            ChannelVariant::Ipc,
            threads_sender.clone(),
            tn.clone(),
        ));

        // Router loops on selecting and sending.
        for _ in 0..iters {
            router_thread_imr.push_back(RecordEntry::new(
                TivoEvent::IpcSelect {
                    select_events: vec![IpcSelectEvent::MessageReceived(1, thread2.clone())],
                },
                ChannelVariant::Ipc,
                router_main_interface.clone(),
                tn.clone(),
            ));

            router_thread_imr.push_back(RecordEntry {
                chan_id: main_recv.clone(),
                ..cb_sender.clone()
            });
        }

        // Router selects and forwards messages.
        for _ in 0..iters {
            router_thread_imr.push_back(RecordEntry::new(
                TivoEvent::IpcSelect {
                    select_events: vec![IpcSelectEvent::MessageReceived(1, thread1.clone())],
                },
                ChannelVariant::Ipc,
                router_main_interface.clone(),
                tn.clone(),
            ));

            router_thread_imr.push_back(RecordEntry::new(
                TivoEvent::CrossbeamSender,
                ChannelVariant::CbUnbounded,
                main_recv.clone(),
                tn.clone(),
            ));
        }

        main_thread_imr.push_back(RecordEntry::new(
            TivoEvent::ThreadInitialized(thread1.clone()),
            ChannelVariant::None,
            DetChannelId::fake(),
            "Thread".to_string(),
        ));

        main_thread_imr.push_back(RecordEntry::new(
            TivoEvent::ThreadInitialized(thread2.clone()),
            ChannelVariant::None,
            DetChannelId::fake(),
            "Thread".to_string(),
        ));

        // Main thread reads messages from router. Thread 2 first.
        for _ in 0..2 * iters {
            main_thread_imr.push_back(
                RecordEntry::new(
                    TivoEvent::CrossbeamRecvSucc {
                        sender_thread: router_thread.clone(),
                    },
                    ChannelVariant::CbUnbounded,
                    main_recv.clone(),
                    tn.clone(),
                )
                .clone(),
            );
        }

        let ipc_sender = RecordEntry::new(
            TivoEvent::IpcSender,
            ChannelVariant::Ipc,
            threads_sender.clone(),
            tn.clone(),
        );
        // Two threads continuously send messages to Router.
        for _ in 0..iters {
            imr1.push_back(ipc_sender.clone());
            imr2.push_back(ipc_sender.clone());
        }

        // Send message to router to wake up.
        main_thread_imr.push_back(RecordEntry {
            chan_id: internal_router_channels.clone(),
            ..ipc_sender
        });

        // Send message to router to send Shutdown message.
        main_thread_imr.push_back(cb_sender.clone());

        // Ack back from Router.
        main_thread_imr.push_back(
            RecordEntry::new(
                TivoEvent::CrossbeamRecvSucc {
                    sender_thread: router_thread.clone(),
                },
                ChannelVariant::CbUnbounded,
                shutdown_ack.clone(),
                tn.clone(),
            )
            .clone(),
        );

        // Shutdown signal arrives
        router_thread_imr.push_back(select.clone());

        // Shutdown event read from channel.
        router_thread_imr.push_back(RecordEntry::new(
            TivoEvent::CrossbeamRecvSucc {
                sender_thread: main_thread.clone(),
            },
            ChannelVariant::CbUnbounded,
            router_main_interface.clone(),
            tn.clone(),
        ));

        // Send back shutdown ack.
        router_thread_imr.push_back(RecordEntry {
            chan_id: shutdown_ack,
            ..cb_sender
        });

        let mut manual_recordlog: HashMap<DetThreadId, VecDeque<RecordEntry>> = HashMap::new();
        manual_recordlog.insert(thread1, imr1);
        manual_recordlog.insert(thread2, imr2);
        manual_recordlog.insert(main_thread, main_thread_imr);
        manual_recordlog.insert(router_thread, router_thread_imr);
        // println!("{:?}", manual_recordlog);
        manual_recordlog
    }

    // Router tests explicitly init the ROUTER to get a
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

            rr_test(|| route_ipc_receiver_to_new_crossbeam_receiver_mpsc(1_000))
        }
    }

    #[test]
    fn router_manual_recordlog_test() -> Result<()> {
        // tracing_subscriber::fmt::Subscriber::builder()
        //     .with_env_filter(EnvFilter::from_default_env())
        //     .with_target(false)
        //     .without_time()
        //     .init();
        Tivo::init_tivo_thread_root_test();

        let iters: i32 = 1_000;
        set_global_memory_replayer(get_manual_recordlog(iters));
        set_rr_mode(RRMode::Replay);

        let results = route_ipc_receiver_to_new_crossbeam_receiver_mpsc(iters)?;
        let mut refv = vec![];
        for i in 0..iters {
            refv.push((2, i));
        }
        for i in 0..iters {
            refv.push((1, i));
        }

        assert_eq!(results, refv);
        Ok(())
    }
    // }
}
