
use crate::message_codec::MessageCodec;

pub trait ServiceHandler<C: MessageCodec>: Clone + Send + Sync + 'static {
    async fn on_request(&self, req: C::RequestView<'_>) -> anyhow::Result<C::Response>;
}