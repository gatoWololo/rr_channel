use serde::{Serialize, Deserialize};

pub struct DetIdSpawner {
    pub child_index: u32,
    pub thread_id: DetThreadId,
}

impl DetIdSpawner {
    pub fn new() -> DetIdSpawner {
        DetIdSpawner { child_index: 0, thread_id: DetThreadId::new()  }
    }

    pub fn new_child_det_id(&mut self) -> DetThreadId {
        let mut new_thread_id = self.thread_id.clone();
        new_thread_id.extend_path(self.child_index);
        self.child_index += 1;
        new_thread_id
    }
}

impl From<DetThreadId> for DetIdSpawner {
    fn from(thread_id :DetThreadId) -> DetIdSpawner {
        DetIdSpawner { child_index: 0, thread_id }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct DetThreadId {
    thread_id: Vec<u32>,
}

impl DetThreadId {
    pub fn new() -> DetThreadId {
        DetThreadId { thread_id: vec![] }
    }

    fn extend_path(&mut self, node: u32) {
        self.thread_id.push(node);
    }
}

impl From<&[u32]> for DetThreadId {
    fn from(thread_id :&[u32]) -> DetThreadId {
        DetThreadId { thread_id: Vec::from(thread_id) }
    }
}
