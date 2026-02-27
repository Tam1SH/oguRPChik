#[cfg(feature = "all")]
#[cfg(test)]
mod tests {
    use ogurpchik::message_codec::MessageCodec;
use super::*;
    use ogurpchik::server::{setup, HasDefaultAllocator};
    use ogurpchik::client::{Client, Priority};
    use ogurpchik::transport::stream::adapters::tcp::TcpTransport;
    use std::ops::Deref;
    use std::time::Duration;
    use rkyv::{Archive, Deserialize, Serialize};
    use tracing::error;
    use ogurpchik::codecs::rkyv_protocol::RkyvCodec;
    use ogurpchik::codecs::serde_compatible::bitcode::BitcodeCodec;
    use ogurpchik::service_handler::ServiceHandler;
    use ogurpchik::transport::base::TransportBuilder;
    use ogurpchik::transport::impls::peer::config::PeerConfig;
    use ogurpchik::transport::stream::adapters::shm::ShmTransport;
    use ogurpchik::transport::stream::adapters::vsock::VsockTransport;
    use ogurpchik::transport::discovery::RpcTopologyRegistry;

    #[derive(Archive, Deserialize, Serialize, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
    #[rkyv(compare(PartialEq), derive(Debug, PartialEq, Eq))]
    pub enum Request {
        Ping,
    }

    #[derive(Archive, Deserialize, Serialize, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
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
                _ => Err(anyhow::anyhow!("Expected Ping, got {:?}", req)),
            }
        }
    }

    macro_rules! rpc_call {
        ($transport:expr, $protocol:ty, $handler:expr, $request:expr) => {{
            let transport = $transport;
            
            let codec_name = <$protocol>::kind();
            let registry = RpcTopologyRegistry::new(transport.kind(), codec_name.to_string());

            let srv_reg = registry.clone();
            let srv_trans = transport.clone();
            let srv_handler = $handler.clone();

            compio::runtime::spawn(async move {
                let Err(e) = setup()
                    .with_transport(srv_trans)
                    .with_registry(srv_reg)
                    .single_thread()
                    .service::<_, $protocol>(srv_handler)
                    .run()
                    .await;

                panic!("server error {}", e);
            }).detach();

            let topology = registry.ready().await;

            let (client, _) = Client::<$protocol, _>::connect(transport, topology)
                .await
                .expect("Failed to connect client");

            client.call($request).await.expect("Call failed")
        }};
    }


    #[compio::test]
    async fn test_rpc_tcp() -> anyhow::Result<()> {
        let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();
        let transport = TcpTransport::new("127.0.0.1".to_string());

        let res = rpc_call!(transport, RkyvCodec<Request, Response>, EchoHandler, Request::Ping);

        assert_eq!(**res.deref(), ArchivedResponse::Pong);
        Ok(())
    }

    #[compio::test]
    async fn test_rpc_vsock() -> anyhow::Result<()> {
        let transport = VsockTransport::server(0, 5000);

        let res = rpc_call!(transport, RkyvCodec<Request, Response>, EchoHandler, Request::Ping);

        assert_eq!(**res.deref(), Response::Pong);
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