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

        crate::detthread::spawn(move || Router::new(msg_receiver, wakeup_receiver).run());
        RouterProxy {
            comm: Mutex::new(RouterProxyComm {
                msg_sender,
                wakeup_sender,
                shutdown: false,
            }),
        }
    }

    /// Send a shutdown message to the router containing a ACK sender,
    /// send a wakeup message to the router, and block on the ACK.
    /// Calling it is idempotent,
    /// which can be useful when running a multi-process system in single-process mode.
    pub fn shutdown(&self) {
        let mut comm = self.comm.lock().unwrap();

        if comm.shutdown {
            return;
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
                ack_receiver.recv().unwrap();
            })
            .unwrap();
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
            .send(RouterMsg::AddRoute(
                receiver.into_opaque(),
                callback_wrapper,
            ))
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
                            RouterMsg::Shutdown(_) => panic!("Shutdown in router! unimplemented!"),
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
