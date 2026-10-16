//! Temporarily lends the active backend to a USB worker, then returns that same backend.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::thread::JoinHandle;

use super::types::{UsbEvent, UsbResult, UsbTask};
use crate::backend::{EmuBackend, placeholder::Placeholder};

pub struct UsbWorker {
    handle: JoinHandle<(Box<dyn EmuBackend>, Result<UsbResult, String>)>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    events: std::sync::mpsc::Sender<UsbEvent>,
}

impl UsbWorker {
    pub fn start(backend: &mut Box<dyn EmuBackend>, task: UsbTask) -> Result<Self, String> {
        let away = Box::new(Placeholder::new(backend.name(), &backend.state_tag()));
        // Keep a recovery owner until the OS successfully creates the thread.
        let slot = Arc::new(Mutex::new(Some(std::mem::replace(backend, away))));
        let worker_slot = slot.clone();
        let cancelled = task.cancelled.clone();
        let events_on_failure = task.events.clone();
        let events = task.events.clone();
        match std::thread::Builder::new()
            .name("evo-usb".into())
            .spawn(move || {
                let mut backend = worker_slot.lock().unwrap().take().unwrap();
                let mut last_update = std::time::Instant::now();
                let mut last_phase = String::new();
                let mut result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    backend
                        .usb(task.operation, &mut |progress| {
                            if last_phase != progress.phase
                                || last_update.elapsed().as_millis() >= 40
                            {
                                last_phase.clone_from(&progress.phase);
                                last_update = std::time::Instant::now();
                                let _ = task.events.send(UsbEvent::Progress(progress));
                            }
                            !task.cancelled.load(Ordering::Relaxed)
                        })
                        .map_err(|e| e.to_string())
                }))
                .unwrap_or_else(|_| {
                    Err("The USB operation stopped unexpectedly. Reopen USB tools to retry.".into())
                });
                // Cancellation and a rejected transfer can still change guest state.
                if let Err(e) = backend.persist() {
                    let message = format!("Calculator changes could not be saved: {e}");
                    result = match result {
                        Ok(UsbResult::Files(files)) => Ok(UsbResult::Batch {
                            files,
                            errors: vec![message],
                            message: "Files received; saving calculator state failed.".into(),
                        }),
                        Ok(UsbResult::Batch {
                            files,
                            mut errors,
                            message: summary,
                        }) => {
                            errors.push(message);
                            Ok(UsbResult::Batch {
                                files,
                                errors,
                                message: summary,
                            })
                        }
                        Err(error) => Err(format!("{error}\n{message}")),
                        _ => Err(message),
                    };
                }
                (backend, result)
            }) {
            Ok(handle) => Ok(Self {
                handle,
                cancelled,
                events,
            }),
            Err(e) => {
                *backend = slot.lock().unwrap().take().unwrap();
                let message = format!("Could not start USB transfer: {e}");
                let _ = events_on_failure.send(UsbEvent::Finished(Err(message.clone())));
                Err(message)
            }
        }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn finish(self) -> Result<Box<dyn EmuBackend>, String> {
        match self.handle.join() {
            Ok((backend, result)) => {
                let _ = self.events.send(UsbEvent::Finished(result));
                Ok(backend)
            }
            Err(_) => {
                let message = "The USB worker was lost".to_owned();
                let _ = self.events.send(UsbEvent::Finished(Err(message.clone())));
                Err(message)
            }
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::connect::types::{ReceivedFile, UsbOperation};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_flash_save_failure_does_not_discard_received_files_or_the_backend() {
        let fake = FakeBackend::new()
            .failing_persist()
            .with_usb_results(vec![Ok(UsbResult::Files(vec![ReceivedFile {
                name: "HELLO.py".into(),
                bytes: b"print(1)".to_vec(),
            }]))]);
        let mut backend: Box<dyn EmuBackend> = Box::new(fake);
        let (events, receiver) = std::sync::mpsc::channel();
        let task = UsbTask {
            operation: UsbOperation::Receive(vec![]),
            events,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let worker = UsbWorker::start(&mut backend, task).unwrap();
        assert!(!backend.usb_available());
        backend = worker.finish().unwrap();
        assert!(backend.usb_available());
        let result = receiver
            .try_iter()
            .find_map(|e| match e {
                UsbEvent::Finished(r) => Some(r),
                _ => None,
            })
            .unwrap()
            .unwrap();
        let UsbResult::Batch { files, errors, .. } = result else {
            panic!("received files were discarded");
        };
        assert_eq!(files[0].bytes, b"print(1)");
        assert!(errors[0].contains("scripted disk error"));
    }
}
