use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct Controller {
    current: watch::Sender<Option<u64>>,
    sequence: Arc<AtomicU64>,
}

impl Controller {
    pub(crate) fn new() -> Self {
        Self {
            current: watch::channel(None).0,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<u64>> {
        self.current.subscribe()
    }

    pub(crate) fn claim(&self, takeover: bool) -> Option<u64> {
        let id = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let claimed = self.current.send_if_modified(|current| {
            if current.is_some() && !takeover {
                return false;
            }

            *current = Some(id);
            true
        });

        claimed.then_some(id)
    }

    pub(crate) fn release(&self, id: u64) {
        self.current.send_if_modified(|current| {
            if *current != Some(id) {
                return false;
            }

            *current = None;
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_revokes_old_owner_and_old_cleanup_cannot_release_new_owner() {
        let controller = Controller::new();
        let current = controller.subscribe();
        let first = controller.claim(false).unwrap();
        assert!(controller.claim(false).is_none());
        let second = controller.claim(true).unwrap();
        assert_ne!(first, second);
        controller.release(first);
        assert_eq!(*current.borrow(), Some(second));
        controller.release(second);
        assert_eq!(*current.borrow(), None);
    }
}
