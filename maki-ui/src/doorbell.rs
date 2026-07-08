use std::sync::Arc;

use arc_swap::{ArcSwapAny, RefCnt};
use flume::{Receiver, Sender};

#[cfg(test)]
use flume::TryRecvError;

/// Wakes the event loop after a producer publishes to its own data channel
/// or slot. bounded(1) so rings collapse: a full doorbell means a wake is
/// already pending, and the loop drains every source per wake.
pub struct Doorbell {
    tx: Sender<()>,
    rx: Receiver<()>,
}

#[derive(Clone)]
pub struct Ringer(Sender<()>);

/// A flume sender that rings after every publish. Producers that feed the
/// event loop hold this instead of a bare Sender, so publish-then-ring
/// cannot be reordered or forgotten.
pub struct NotifyingSender<T> {
    data: Sender<T>,
    bell: Ringer,
}

/// An ArcSwap slot that rings after every store. Generic over the swapped
/// pointer so it covers both ArcSwap<T> (A = Arc<T>) and ArcSwapOption<T>
/// (A = Option<Arc<T>>).
pub struct NotifyingSlot<A: RefCnt> {
    slot: Arc<ArcSwapAny<A>>,
    bell: Ringer,
}

impl Doorbell {
    pub fn new() -> Self {
        let (tx, rx) = flume::bounded(1);
        Self { tx, rx }
    }

    pub fn ringer(&self) -> Ringer {
        Ringer(self.tx.clone())
    }

    pub fn receiver(&self) -> &Receiver<()> {
        &self.rx
    }
}

impl Default for Doorbell {
    fn default() -> Self {
        Self::new()
    }
}

impl Ringer {
    pub fn ring(&self) {
        let _ = self.0.try_send(());
    }

    /// A ringer whose rings go nowhere. For constructing UI types in tests.
    pub fn disconnected() -> Self {
        let (tx, _) = flume::bounded(1);
        Self(tx)
    }
}

impl<T> NotifyingSender<T> {
    pub fn new(data: Sender<T>, bell: Ringer) -> Self {
        Self { data, bell }
    }

    #[allow(dead_code)]
    pub fn send(&self, value: T) {
        let _ = self.data.send(value);
        self.bell.ring();
    }

    pub fn try_send(&self, value: T) -> Result<(), flume::TrySendError<T>> {
        let res = self.data.try_send(value);
        self.bell.ring();
        res
    }
}

impl<A: RefCnt> NotifyingSlot<A> {
    pub fn new(slot: Arc<ArcSwapAny<A>>, bell: Ringer) -> Self {
        Self { slot, bell }
    }

    pub fn slot(&self) -> &ArcSwapAny<A> {
        &self.slot
    }

    pub fn store(&self, value: A) {
        self.slot.store(value);
        self.bell.ring();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_swap::ArcSwap;

    #[test]
    fn ring_when_empty_makes_one_pending() {
        let bell = Doorbell::new();
        bell.ringer().ring();
        assert_eq!(bell.receiver().try_recv(), Ok(()));
        assert_eq!(bell.receiver().try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn rings_collapse() {
        let bell = Doorbell::new();
        let ringer = bell.ringer();
        ringer.ring();
        ringer.ring();
        ringer.ring();
        assert_eq!(bell.receiver().try_recv(), Ok(()));
        assert_eq!(bell.receiver().try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn notifying_sender_publishes_before_ring() {
        let bell = Doorbell::new();
        let (data_tx, data_rx) = flume::unbounded::<u32>();
        let sender = NotifyingSender::new(data_tx, bell.ringer());
        sender.send(42);
        assert_eq!(bell.receiver().try_recv(), Ok(()));
        assert_eq!(data_rx.try_recv(), Ok(42));
    }

    #[test]
    fn notifying_slot_stores_before_ring() {
        let bell = Doorbell::new();
        let slot: Arc<ArcSwap<u32>> = Arc::new(ArcSwap::from_pointee(0));
        let notifying = NotifyingSlot::new(slot.clone(), bell.ringer());
        notifying.store(Arc::new(7));
        assert_eq!(bell.receiver().try_recv(), Ok(()));
        assert_eq!(**slot.load(), 7);
    }

    #[test]
    fn disconnected_ringer_is_inert() {
        Ringer::disconnected().ring();
    }
}
