use backend;

enum EventKind {
  Send,
  Receive,
}

pub struct Event {
  pub kind: EventKind,
  pub time: String,
  pub valid: bool,
  pub task: String,
  pub service: String,
  pub identity: String,
}

pub fn register_event(e: Event) {}
