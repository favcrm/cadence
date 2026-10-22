//! Unit-test observation of real SQLite contention and initialization retries.
//! Opt-in per calling thread; ordinary connections retain their busy timeout.
use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender};

thread_local! {
    pub(crate) static WAITING: RefCell<Option<Sender<()>>> = const { RefCell::new(None) };
    pub(crate) static RELEASED: RefCell<Option<Receiver<()>>> = const { RefCell::new(None) };
}

pub(crate) fn waiting() {
    WAITING.with(|observer| {
        if let Some(sender) = observer.borrow_mut().take() {
            let _ = sender.send(());
        }
    });
}

pub(crate) fn configure(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    if WAITING.with(|observer| observer.borrow().is_some())
        && RELEASED.with(|release| release.borrow().is_some())
    {
        // Keep the real native lock and the same outer busy bound. The observer
        // releases the fixture's competing writer only once SQLite reports busy.
        conn.busy_handler(Some(|_| {
            waiting();
            RELEASED.with(|release| {
                release
                    .borrow_mut()
                    .take()
                    .is_some_and(|receiver| receiver.recv_timeout(super::BUSY_TIMEOUT).is_ok())
            })
        }))?;
    }
    Ok(())
}
