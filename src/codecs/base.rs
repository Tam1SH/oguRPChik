use anyhow::Result;

pub enum Envelope<Req, Res> {
    Request { id: u64, payload: Req },
    Response { id: u64, payload: Res },
    Event { payload: Req },
}

pub trait MessageCodec: Send + Sync + 'static {
    type Request: Send + Sync + 'static;
    type Response: Send + Sync + 'static;

    type RequestView<'a>: Send + Sync;
    type ResponseView<'a>: Send + Sync;
    type Dest: AsRef<[u8]> + Default + Send + Sync + 'static;

    fn decode(data: &[u8]) -> Result<Envelope<Self::RequestView<'_>, Self::ResponseView<'_>>>;

    fn encode(msg: Envelope<Self::Request, Self::Response>, dest: &mut Self::Dest) -> Result<()>;

    fn kind() -> &'static str;
}
