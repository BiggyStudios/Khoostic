//! [`Pending`]: a result being computed elsewhere (a thread, an async task), polled by UIs
//! once per frame without blocking.

use crossbeam_channel::{Receiver, Sender};

pub struct Pending<T>(Receiver<anyhow::Result<T>>);

/// The producing side of a [`Pending`].
pub struct Resolver<T>(Sender<anyhow::Result<T>>);

impl<T> Resolver<T> {
    pub fn resolve(self, value: anyhow::Result<T>) {
        let _ = self.0.send(value);
    }
}

impl<T> Pending<T> {
    pub fn new() -> (Resolver<T>, Pending<T>) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        (Resolver(tx), Pending(rx))
    }

    /// Runs `f` on a new thread.
    pub fn spawn(name: &str, f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Self
    where
        T: Send + 'static,
    {
        let (tx, rx) = Self::new();
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || tx.resolve(f()));
        if let Err(e) = spawned {
            let (tx, rx) = Self::new();
            tx.resolve(Err(e.into()));
            return rx;
        }
        rx
    }

    /// An already finished result.
    pub fn ready(value: anyhow::Result<T>) -> Self {
        let (tx, rx) = Self::new();
        tx.resolve(value);
        rx
    }

    /// The result, if it has arrived. Returns `Some` once.
    pub fn poll(&self) -> Option<anyhow::Result<T>> {
        self.0.try_recv().ok()
    }

    pub fn wait(self) -> anyhow::Result<T> {
        self.0
            .recv()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("background worker stopped")))
    }
}
