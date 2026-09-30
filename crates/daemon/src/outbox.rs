//! Per-client outgoing frame queue: strict ordering, and a byte budget for pane data so a slow
//! client cannot make the daemon buffer output without limit.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use protocol::frame::Frame;
use tokio::sync::Notify;

#[derive(Default)]
struct Queue {
    frames: VecDeque<Frame>,
    data_bytes: usize,
    closed: bool,
}

/// Ordered frame queue shared by the daemon (producers) and one connection writer (consumer).
#[derive(Clone)]
pub struct Outbox {
    queue: Arc<Mutex<Queue>>,
    ready: Arc<Notify>,
    data_budget: usize,
}

impl Outbox {
    /// Queue that holds at most `data_budget` bytes of pane data.
    pub fn new(data_budget: usize) -> Outbox {
        Outbox { queue: Arc::default(), ready: Arc::new(Notify::new()), data_budget }
    }

    /// Queue a control frame; always accepted.
    pub fn push_control(&self, frame: Frame) {
        self.queue.lock().unwrap().frames.push_back(frame);
        self.ready.notify_one();
    }

    /// Queue a data frame unless that would exceed the data budget.
    pub fn try_push_data(&self, frame: Frame) -> bool {
        let mut queue = self.queue.lock().unwrap();
        let len = frame.encoded_len();
        if queue.data_bytes + len > self.data_budget {
            return false;
        }
        queue.data_bytes += len;
        queue.frames.push_back(frame);
        drop(queue);
        self.ready.notify_one();
        true
    }

    /// Whether at most half the data budget is in use (safe to start a resync).
    pub fn has_room(&self) -> bool {
        self.queue.lock().unwrap().data_bytes <= self.data_budget / 2
    }

    /// Stop accepting new frames after the queued ones are delivered.
    pub fn close(&self) {
        self.queue.lock().unwrap().closed = true;
        self.ready.notify_one();
    }

    /// Next frame, waiting if empty; `None` once closed and drained.
    pub async fn next(&self) -> Option<Frame> {
        loop {
            {
                let mut queue = self.queue.lock().unwrap();
                if let Some(frame) = queue.frames.pop_front() {
                    if matches!(frame, Frame::Data { .. }) {
                        queue.data_bytes -= frame.encoded_len();
                    }
                    return Some(frame);
                }
                if queue.closed {
                    return None;
                }
            }
            self.ready.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize) -> Frame {
        Frame::Data { pane: 1, bytes: vec![0; n] }
    }

    #[tokio::test]
    async fn frames_keep_push_order() {
        let outbox = Outbox::new(1000);
        outbox.push_control(Frame::Control(b"a".to_vec()));
        assert!(outbox.try_push_data(data(10)));
        outbox.push_control(Frame::Control(b"b".to_vec()));
        assert_eq!(outbox.next().await, Some(Frame::Control(b"a".to_vec())));
        assert_eq!(outbox.next().await, Some(data(10)));
        assert_eq!(outbox.next().await, Some(Frame::Control(b"b".to_vec())));
    }

    #[tokio::test]
    async fn data_over_budget_is_refused_but_control_is_not() {
        let outbox = Outbox::new(1000);
        assert!(outbox.try_push_data(data(600)));
        assert!(!outbox.try_push_data(data(600)));
        assert!(!outbox.has_room());
        outbox.push_control(Frame::Control(b"exited".to_vec()));
        assert_eq!(outbox.next().await, Some(data(600)));
        assert!(outbox.has_room());
        assert!(outbox.try_push_data(data(600)));
    }

    #[tokio::test]
    async fn close_drains_then_ends() {
        let outbox = Outbox::new(1000);
        outbox.push_control(Frame::Control(b"last".to_vec()));
        outbox.close();
        assert_eq!(outbox.next().await, Some(Frame::Control(b"last".to_vec())));
        assert_eq!(outbox.next().await, None);
    }
}
