//! Actual protected daemon maintenance, outside RPC/caller JSON. The private
//! constructor-created channel carries selectors only; StoreOwnerGrant still
//! requires authentic root/external one-use consumption before SQLite effects.
use super::*;
use crate::installer_bundle::constructor::{
    private_wire::{self, Packet},
    runtime_child,
};
use std::sync::mpsc;
struct Quiesced;
struct Failed {
    shared: Arc<Shared>,
    control: Arc<std::os::unix::net::UnixDatagram>,
}
impl Drop for Failed {
    fn drop(&mut self) {
        // This worker normally remains serving. Error/panic never leaves a
        // healthy-looking daemon with a silently lost native control reader.
        self.shared.begin_closing();
        if private_wire::send_child(&self.control, &Packet::Failed { version: 1 }).is_err() {
            unsafe { libc::_exit(125) }
        }
    }
}
// Real bound listeners are transferred only from serve_with AFTER protected
// Store startup/recovery and actor/control setup. No caller JSON/ready flag.
pub(super) struct ServingLifetime {
    control: Arc<std::os::unix::net::UnixDatagram>,
}
pub(super) fn publish_serving(
    private: &std::os::unix::net::UnixListener,
    shared: &std::os::unix::net::UnixListener,
) -> Result<ServingLifetime> {
    let control = crate::installer_bundle::constructor::runtime_child::control()
        .ok_or_else(|| Error::rejected("actual protected runtime control unavailable"))?;
    private_wire::send_serving(&control, private, shared)?;
    Ok(ServingLifetime { control })
}
impl Drop for ServingLifetime {
    fn drop(&mut self) {
        // Terminal invalidation only; never used as evidence of retirement/FINAL.
        if private_wire::send_child(&self.control, &Packet::ServingStopped { version: 1 }).is_err()
        {
            unsafe { libc::_exit(125) }
        }
    }
}
pub(super) struct Maintenance {
    completed: mpsc::SyncSender<Quiesced>,
    worker: thread::JoinHandle<Result<()>>,
}
impl Maintenance {
    pub(super) fn start(shared: &Arc<Shared>) -> Option<Self> {
        let control = runtime_child::control()?;
        let shared = shared.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || -> Result<()> {
            let _failure = Failed {
                shared: shared.clone(),
                control: control.clone(),
            };
            let mut closed = false;
            let mut task = None::<super::native_task::Task>;
            let mut task_spent = false;
            let mut registrations = super::native_task::Registrations::new(&shared.store);
            loop {
                let packet = match private_wire::receive_child(&control)? {
                    Some(p) => p,
                    None => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                };
                match packet {
                    Packet::Task {
                        version: 1,
                        task: id,
                        alias,
                        model,
                        prompt,
                    } if !closed && !task_spent => {
                        task_spent = true;
                        task = Some(super::native_task::Task::start(
                            &shared,
                            control.clone(),
                            id,
                            alias,
                            model,
                            prompt,
                            &mut registrations,
                        )?);
                    }
                    Packet::Cancel {
                        version: 1,
                        task: id,
                    } if !closed => {
                        let current = task
                            .as_ref()
                            .ok_or_else(|| Error::unknown("native task unavailable"))?;
                        if current.id != id {
                            return Err(Error::rejected("native cancellation task changed"));
                        }
                        current.interrupt();
                    }
                    Packet::Retire {
                        version: 1,
                        task: id,
                    } if !closed => {
                        let current = task
                            .take()
                            .ok_or_else(|| Error::unknown("native task unavailable"))?;
                        if current.id != id {
                            return Err(Error::rejected("native retirement task changed"));
                        }
                        current.retire()?;
                        private_wire::send_child(
                            &control,
                            &Packet::Retired {
                                version: 1,
                                task: id,
                            },
                        )?;
                        task_spent = false; // Root history/current/retirement admits the next generation.
                    }
                    Packet::Close {
                        version: 1,
                        binding,
                    } if !closed => {
                        if binding.purpose != crate::store::Purpose::Close {
                            return Err(Error::rejected("protected close scope refused"));
                        }
                        // Drain/join real actors and flush BEFORE asking the
                        // owner to elect/burn close; no expired opening renewal.
                        if let Some(current) = task.take() {
                            current.retire()?;
                        }
                        shared.begin_closing();
                        rx.recv_timeout(Duration::from_secs(300))
                            .map_err(|_| Error::unknown("protected drain/flush UNKNOWN"))?;
                        let attempt = binding.attempt.clone();
                        let grant = crate::store::StoreOwnerGrant::acquire(binding)?;
                        shared
                            .store
                            .close_owned(grant, "protected runtime owner close")?;
                        private_wire::send_child(
                            &control,
                            &Packet::Closed {
                                version: 1,
                                attempt,
                            },
                        )?;
                        closed = true;
                    }
                    Packet::Witness {
                        version: 1,
                        binding,
                    } if closed => {
                        if binding.purpose != crate::store::Purpose::Witness {
                            return Err(Error::rejected("protected witness scope refused"));
                        }
                        let grant = crate::store::StoreOwnerGrant::acquire(binding)?;
                        let witness = shared.store.witness_owned(grant)?;
                        // Serialize ONLY the actual committed typed return.
                        private_wire::send_child(
                            &control,
                            &Packet::Witnessed {
                                version: 1,
                                witness: serde_json::to_value(witness)?,
                            },
                        )?;
                    }
                    _ => {
                        return Err(Error::rejected(
                            "protected maintenance phase/replay refused",
                        ))
                    }
                }
            }
        });
        Some(Self {
            completed: tx,
            worker,
        })
    }
    /// Called ONLY on the actual successful joined shutdown + flush tail.
    pub(super) fn quiesced(self) -> Result<()> {
        self.completed
            .send(Quiesced)
            .map_err(|_| Error::unknown("protected maintenance peer lost"))?;
        // Remain a quiesced-RUNNING owned process for witness/capture. EOF,
        // cancellation/physical cleanup is never a durable FINAL assertion.
        self.worker
            .join()
            .map_err(|_| Error::unknown("protected maintenance worker lost"))?
    }
}
