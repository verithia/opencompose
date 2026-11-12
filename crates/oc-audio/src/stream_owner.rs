//! Keep thread-affine native resources on their construction thread. Only a
//! shutdown receipt and plain construction metadata cross the thread boundary.

use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

pub(crate) struct StreamOwner {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl StreamOwner {
    pub(crate) fn open<T: 'static, R: Send + 'static>(
        name: &str,
        create: impl FnOnce() -> Result<(T, R), String> + Send + 'static,
    ) -> Result<(Self, R), String> {
        let (ready, result) = mpsc::sync_channel(0);
        let (stop, stopped) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || match create() {
                Ok((resource, metadata)) => {
                    if ready.send(Ok(metadata)).is_ok() {
                        let _ = stopped.recv();
                    }
                    drop(resource);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|error| format!("Could not start the audio stream owner: {error}"))?;
        match result.recv() {
            Ok(Ok(metadata)) => Ok((
                Self {
                    stop: Some(stop),
                    worker: Some(worker),
                },
                metadata,
            )),
            failure => {
                drop(stop);
                let _ = worker.join();
                Err(match failure {
                    Ok(Err(error)) => error,
                    _ => "The audio stream owner stopped during initialization".to_owned(),
                })
            }
        }
    }
}

impl Drop for StreamOwner {
    fn drop(&mut self) {
        // Disconnect before joining: native teardown runs on the owner, never
        // on a GUI/settings thread that happens to replace the stream handle.
        drop(self.stop.take());
        if let Some(worker) = self.worker.take()
            && worker.thread().id() != std::thread::current().id()
            && worker.join().is_err()
        {
            log::error!("The audio stream owner panicked during teardown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::thread::ThreadId;

    struct ThreadAffineResource {
        owner: ThreadId,
        _not_send: Rc<()>,
        destroyed: Sender<ThreadId>,
    }
    impl Drop for ThreadAffineResource {
        fn drop(&mut self) {
            assert_eq!(self.owner, std::thread::current().id());
            self.destroyed.send(self.owner).unwrap();
        }
    }

    #[test]
    fn stream_owner_never_moves_a_non_send_resource_and_joins_its_destruction() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StreamOwner>();
        let (destroyed, destruction) = mpsc::channel();
        let (owner, construction_thread) = StreamOwner::open("test-stream-owner", move || {
            let thread = std::thread::current().id();
            Ok((
                ThreadAffineResource {
                    owner: thread,
                    _not_send: Rc::new(()),
                    destroyed,
                },
                thread,
            ))
        })
        .unwrap();
        assert_ne!(construction_thread, std::thread::current().id());
        assert!(destruction.try_recv().is_err());
        std::thread::spawn(move || drop(owner)).join().unwrap();
        assert_eq!(destruction.try_recv().unwrap(), construction_thread);
        assert!(destruction.try_recv().is_err());
    }

    #[test]
    fn stream_owner_setup_failure_retires_partial_resources_on_the_owner() {
        let (destroyed, destruction) = mpsc::channel();
        let failure =
            StreamOwner::open::<ThreadAffineResource, ()>("test-stream-error", move || {
                let _resource = ThreadAffineResource {
                    owner: std::thread::current().id(),
                    _not_send: Rc::new(()),
                    destroyed,
                };
                Err("device refused the requested format".to_owned())
            });
        assert_eq!(
            failure.err().unwrap(),
            "device refused the requested format"
        );
        assert_ne!(destruction.try_recv().unwrap(), std::thread::current().id());
    }

    #[test]
    fn stream_owner_initialization_panic_does_not_leave_a_pending_receipt() {
        let failure = StreamOwner::open::<(), ()>("test-stream-panic", || panic!("fixture panic"));
        assert!(
            failure
                .err()
                .unwrap()
                .contains("stopped during initialization")
        );
    }
}
