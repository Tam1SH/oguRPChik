use anyhow::Result;


pub enum Envelope<Req, Res> {
    Request { id: u64, payload: Req },
    Response { id: u64, payload: Res },
    Push { payload: Req },
}


pub trait MessageCodec: Send + Sync + 'static {
    type Request: Send + Sync + 'static;
    type Response: Send + Sync + 'static;

    type RequestView: ?Sized + Send + Sync;
    type ResponseView: ?Sized + Send + Sync;
    type Dest: AsRef<[u8]> + Send + Sync + 'static;

    fn decode(data: &[u8]) -> Result<Envelope<&Self::RequestView, &Self::ResponseView>>;

    fn encode(
        msg: Envelope<Self::Request, Self::Response>,
        dest: &mut Self::Dest,
    ) -> Result<()>;
    
    fn kind() -> &'static str;
}


