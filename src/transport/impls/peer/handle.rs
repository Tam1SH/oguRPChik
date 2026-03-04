use crate::align_buffer::AlignedBuffer;
use local_sync::mpsc::bounded::Tx;
use smallvec::SmallVec;

use crate::transport::base::{MessageSink, MessageSource};

pub type MsgBatch = SmallVec<[AlignedBuffer; 8]>;
pub type IncomingMsg = SmallVec<[AlignedBuffer; 8]>;

pub enum OutgoingMsg {
    Single(AlignedBuffer),
    Batch(MsgBatch),
}

impl MessageSink for PeerSink {
    type Payload = AlignedBuffer;

    async fn send(&self, data: AlignedBuffer) -> anyhow::Result<()> {
        PeerSink::send(self, data).await
    }
}

impl MessageSource for PeerSource {
    type Payload = AlignedBuffer;
    async fn recv(&mut self) -> Option<AlignedBuffer> {
        self.incoming_rx.recv().await
    }
}

#[derive(Clone)]
pub struct PeerSink {
    pub outgoing_tx: Tx<OutgoingMsg>,
}

pub struct PeerSource {
    pub incoming_rx: local_sync::mpsc::bounded::Rx<AlignedBuffer>,
}

impl PeerSink {
    pub async fn send(&self, data: AlignedBuffer) -> anyhow::Result<()> {
        self.outgoing_tx
            .send(OutgoingMsg::Single(data))
            .await
            .map_err(|e| anyhow::anyhow!("transport dead: {:?}", e))
    }

    pub async fn send_batch(&self, msgs: MsgBatch) -> anyhow::Result<()> {
        self.outgoing_tx
            .send(OutgoingMsg::Batch(msgs))
            .await
            .map_err(|e| anyhow::anyhow!("transport dead: {:?}", e))
    }
}
