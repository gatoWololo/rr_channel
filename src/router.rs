// Copyright 2015 The Servo Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::sync::Mutex;
use crate::thread;
use crate::thread::DetThreadId;
use crate::log_trace;
use crate::thread::get_det_id;
use crate::thread::set_det_id;

// use crate::ipc::OpaqueIpcReceiver;
use ipc_channel::ipc::OpaqueIpcReceiver;
use crate::ipc::{
    self, IpcReceiver, IpcReceiverSet, IpcSelectionResult, IpcSender, OpaqueIpcMessage,
};
use crate::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use lazy_static::lazy_static;

lazy_static! {
    pub static ref ROUTER: RouterProxy = RouterProxy::new();
}

/// OMAR: Must wrap to use our channel type.
pub struct RouterProxy {
    comm: Mutex<RouterProxyComm>,
}

impl RouterProxy {
    pub fn new() -> RouterProxy {
        let (msg_sender, msg_receiver) = crate::unbounded();
        let (wakeup_sender, wakeup_receiver) = ipc::channel().unwrap();
        thread::spawn(move || Router::new(msg_receiver, wakeup_receiver).run());
        RouterProxy {
            comm: Mutex::new(RouterProxyComm {
                msg_sender: msg_sender,
                wakeup_sender: wakeup_sender,
            }),
        }
    }

    // pub fn add_route(&self, receiver: OpaqueIpcReceiver, callback: RouterHandler) {
    //     let comm = self.comm.lock().unwrap();
    //     comm.msg_sender
    //         .send(RouterMsg::AddRoute(receiver, callback))
    //         .unwrap();
    //     comm.wakeup_sender.send(()).unwrap();
    // }

    pub fn add_route<T: 'static>(&self, receiver: IpcReceiver<T>,
                              mut callback: Box<FnMut(Result<T, ipc_channel::Error>) + Send>)
    where T: for<'de> Deserialize<'de> + Serialize {
        let comm = self.comm.lock().unwrap();

        let callback_wrapper = Box::new(move |msg: OpaqueIpcMessage| {
            match msg.to::<(Option<DetThreadId>, T)>() {
                Ok((forward_id, msg)) => {
                    // Big Hack: Temporarily set TLS DetThreadId so original sender's
                    // DetThreadId is properly forwarded to receiver.
                    let original_id = get_det_id().expect("Router threadId not set?");
                    set_det_id(forward_id.expect("Forward Id was None..."));
                    callback(Ok(msg));
                    set_det_id(original_id);
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
        log_trace(&format!("Routing IpcReceiver<{:?}> to crossbeam_sender: {:?}",
                           ipc_receiver.metadata.id, crossbeam_sender.channel_id));
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
        log_trace(&format!("Routing IpcReceiver<{:?}>", ipc_receiver.metadata.id));

        let (crossbeam_sender, crossbeam_receiver) = crate::unbounded();
        log_trace(&format!("Created Channels<{:?} for routing.", crossbeam_receiver.metadata.id));

        self.route_ipc_receiver_to_crossbeam_sender(ipc_receiver, crossbeam_sender);
        crossbeam_receiver
    }
}

/// OMAR: Must wrap since we need it to use our rr channels.
struct RouterProxyComm {
    msg_sender: Sender<RouterMsg>,
    wakeup_sender: IpcSender<()>,
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
            msg_receiver: msg_receiver,
            msg_wakeup_id: msg_wakeup_id,
            ipc_receiver_set: ipc_receiver_set,
            handlers: HashMap::new(),
        }
    }

    fn run(&mut self) {
        loop {
            let results = match self.ipc_receiver_set.select() {
                Ok(results) => results,
                Err(_) => break,
            };
            for result in results.into_iter() {
                match result {
                    IpcSelectionResult::MessageReceived(id, _) if id == self.msg_wakeup_id =>
                        match self.msg_receiver.recv().unwrap() {
                            RouterMsg::AddRoute(receiver, handler) => {
                                let new_receiver_id =
                                    self.ipc_receiver_set.add_opaque(receiver).unwrap();
                                self.handlers.insert(new_receiver_id, handler);
                            },
                        },
                    IpcSelectionResult::MessageReceived(id, message) =>
                        self.handlers.get_mut(&id).unwrap()(message),
                    IpcSelectionResult::ChannelClosed(id) => {
                        self.handlers.remove(&id).unwrap();
                    },
                }
            }
        }
    }
}

enum RouterMsg {
    AddRoute(OpaqueIpcReceiver, RouterHandler),
}

pub type RouterHandler = Box<FnMut(OpaqueIpcMessage) + Send>;
