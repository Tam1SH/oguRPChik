use crate::codecs::base::{BorrowedBuf, OwnedBuf};
use crate::transport::base::{MessageSink, MessageSource};
use local_sync::mpsc::bounded::{Rx, Tx};
use smallvec::SmallVec;

pub type MsgBatch<B> = SmallVec<[B; 8]>;

pub enum OutgoingMsg<B: OwnedBuf> {
    Single(B),
    Batch(MsgBatch<B>),
}

#[derive(Clone)]
pub struct PeerSink<B: OwnedBuf> {
    pub outgoing_tx: Tx<OutgoingMsg<B>>,
}

pub struct PeerSource<B: BorrowedBuf> {
    pub incoming_rx: Rx<B>,
}

impl<B: OwnedBuf> MessageSink for PeerSink<B> {
    type Payload = B;

    async fn send(&self, data: B) -> anyhow::Result<()> {
        self.outgoing_tx
            .send(OutgoingMsg::Single(data))
            .await
            .map_err(|e| anyhow::anyhow!("transport dead: {:?}", e))
    }
}

impl<B: BorrowedBuf> MessageSource for PeerSource<B> {
    type Payload = B;

    async fn recv(&mut self) -> Option<B> {
        self.incoming_rx.recv().await
    }
}

impl<B: OwnedBuf> PeerSink<B> {
    pub async fn send_batch(&self, msgs: MsgBatch<B>) -> anyhow::Result<()> {
        self.outgoing_tx
            .send(OutgoingMsg::Batch(msgs))
            .await
            .map_err(|e| anyhow::anyhow!("transport dead: {:?}", e))
    }
}
