//! Fixture-scoped completion witnesses, compiled out of release builds.
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    ImageWorkerEnded,
    ImageAttemptFailed,
    AppRunsAdvanced,
}

struct Observer {
    state_dir: PathBuf,
    sender: Arc<mpsc::Sender<Event>>,
}

static OBSERVERS: Mutex<Vec<Observer>> = Mutex::new(Vec::new());

pub struct Subscription {
    receiver: mpsc::Receiver<Event>,
    sender: Arc<mpsc::Sender<Event>>,
}

pub fn subscribe(state_dir: &Path) -> Subscription {
    let (sender, receiver) = mpsc::channel();
    let sender = Arc::new(sender);
    OBSERVERS.lock().unwrap().push(Observer {
        state_dir: state_dir.to_owned(),
        sender: sender.clone(),
    });
    Subscription { receiver, sender }
}

impl Subscription {
    /// One deadline bounds the entire wait, including unrelated events.
    pub fn wait_for(
        &self,
        event: Event,
        count: usize,
        timeout: Duration,
    ) -> Result<(), mpsc::RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut remaining = count;
        while remaining > 0 {
            if self
                .receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?
                == event
            {
                remaining -= 1;
            }
        }
        Ok(())
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        OBSERVERS
            .lock()
            .unwrap()
            .retain(|observer| !Arc::ptr_eq(&observer.sender, &self.sender));
    }
}

pub(super) fn completed(state_dir: &Path, event: Event) {
    for observer in OBSERVERS.lock().unwrap().iter() {
        if observer.state_dir == state_dir {
            let _ = observer.sender.send(event);
        }
    }
}
