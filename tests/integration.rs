#[cfg(feature = "all")]
#[cfg(test)]
mod tests {
    use ogurpchik::codecs::rkyv_protocol::RkyvCodec;
    use ogurpchik::codecs::serde_compatible::bitcode::BitcodeCodec;
    use ogurpchik::high::node::Node;
    use ogurpchik::high::service_handler::ServiceHandler;
    use ogurpchik::transport::stream::adapters::shm::ShmTransport;
    #[cfg(all(feature = "npipe", windows))]
    use ogurpchik::transport::stream::adapters::npipe::NamedPipeTransport;
    use ogurpchik::transport::stream::adapters::tcp::TcpTransport;
    use ogurpchik::transport::stream::adapters::vsock::{VsockAddr, VsockTransport};
    use rkyv::{Archive, Deserialize, Serialize};
    use std::ops::Deref;

    #[derive(
        Archive, Deserialize, Serialize, Debug, PartialEq, serde::Deserialize, serde::Serialize,
    )]
    #[rkyv(compare(PartialEq), derive(Debug, PartialEq, Eq))]
    pub enum Request {
        Ping,
    }

    #[derive(
        Archive, Deserialize, Serialize, Debug, PartialEq, serde::Deserialize, serde::Serialize,
    )]
    #[rkyv(compare(PartialEq), derive(Debug, PartialEq, Eq))]
    pub enum Response {
        Pong,
    }

    #[derive(Clone)]
    struct EchoHandler;

    impl ServiceHandler<RkyvCodec<Request, Response>> for EchoHandler {
        async fn on_request<'a>(&self, req: &ArchivedRequest) -> anyhow::Result<Response> {
            match req {
                ArchivedRequest::Ping => Ok(Response::Pong),
            }
        }
    }

    impl ServiceHandler<BitcodeCodec<Request, Response>> for EchoHandler {
        async fn on_request<'a>(&self, req: Request) -> anyhow::Result<Response> {
            match req {
                Request::Ping => Ok(Response::Pong),
            }
        }
    }

    macro_rules! rpc_call {
        ($transport:expr, $protocol:ty, $handler:expr, $request:expr) => {{
            let transport = $transport;

            let (client, _guard) = Node::new()?
                .serve::<$protocol, _, _>(transport.clone(), $handler.clone())
                .connect::<$protocol, _>(transport)
                .start()
                .await
                .expect("Node start failed");

            client.call($request).await.expect("Call failed")
        }};
    }

    #[compio::test]
    async fn test_rpc_tcp() -> anyhow::Result<()> {
        let transport = TcpTransport::new("127.0.0.1".to_string());

        let res = rpc_call!(transport, RkyvCodec<Request, Response>, EchoHandler, Request::Ping);

        assert_eq!(**res.deref(), ArchivedResponse::Pong);
        Ok(())
    }

    #[compio::test]
    async fn test_rpc_vsock() -> anyhow::Result<()> {
        let transport = VsockTransport::server(VsockAddr::Cid(0), 5000);

        let res = rpc_call!(transport, RkyvCodec<Request, Response>, EchoHandler, Request::Ping);

        assert_eq!(**res.deref(), Response::Pong);
        Ok(())
    }

    #[cfg(all(feature = "npipe", windows))]
    #[compio::test]
    async fn test_rpc_named_pipe() -> anyhow::Result<()> {
        let transport = NamedPipeTransport::new(format!("ogurpchik-rpc-test-{}", std::process::id()));

        let res = rpc_call!(transport, BitcodeCodec<Request, Response>, EchoHandler, Request::Ping);

        assert_eq!(*res.deref(), Response::Pong);
        Ok(())
    }

    #[compio::test]
    async fn test_rpc_shm() -> anyhow::Result<()> {
        let service_base_name = format!("test_shm_{}", std::process::id());
        let transport = ShmTransport::new(&service_base_name);

        let res = rpc_call!(transport, BitcodeCodec<Request, Response>, EchoHandler, Request::Ping);

        assert_eq!(*res.deref(), Response::Pong);
        Ok(())
    }
}
