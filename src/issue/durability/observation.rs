//! cfg(test)-only synchronous observation of ACTUAL origin capture and accepted
//! reservation. Independent QA owns observers, spools, labels and assertions.
use super::{Receipt, Reservation};
use std::cell::RefCell;
use std::sync::Arc;

pub trait Observer: Send + Sync {
    /// Called under the original PmLock, before any reserve transport. Bytes
    /// are the actual frozen bundle; QA may save its own owned observation.
    fn captured(&self, receipt: &Receipt, artifact: &[u8]);
    /// Called under the SAME PmLock only after accepted proof/deadline checks.
    fn reserved(&self, reservation: &Reservation);
}
thread_local! {
    static OBSERVER: RefCell<Option<Arc<dyn Observer>>> = const { RefCell::new(None) };
}
pub fn with_observer<T>(observer: Arc<dyn Observer>, operation: impl FnOnce() -> T) -> T {
    struct Reset(Option<Arc<dyn Observer>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVER.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let previous = OBSERVER.with(|slot| slot.borrow_mut().replace(observer));
    let _reset = Reset(previous);
    operation()
}
pub(super) fn captured(receipt: &Receipt, artifact: &[u8]) {
    let observer = OBSERVER.with(|slot| slot.borrow().clone());
    if let Some(observer) = observer {
        observer.captured(receipt, artifact);
    }
}
pub(super) fn reserved(reservation: &Reservation) {
    let observer = OBSERVER.with(|slot| slot.borrow().clone());
    if let Some(observer) = observer {
        observer.reserved(reservation);
    }
}
