//! `external_streams_with` should spawn one WS handshake per incoming TCP
//! so a slow upgrade doesn't head-of-line block other clients.

use std::time::Duration;

use sunset_sync::{RawTransport, Result as SyncResult};
use sunset_sync_ws_native::WebSocketRawTransport;

#[tokio::test(flavor = "current_thread")]
async fn parallel_ws_handshakes() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Bind a pair of sides — the test acts as both the
            // "router" feeding TcpStreams onto the channel AND as a
            // pool of clients dialing those streams.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();

            let (tcp_tx, tcp_rx) = tokio::sync::mpsc::channel::<tokio::net::TcpStream>(64);

            // Spawn the listener-feeder.
            let _feeder = tokio::task::spawn_local(async move {
                loop {
                    let (server_tcp, _peer) = listener.accept().await.unwrap();
                    if tcp_tx.send(server_tcp).await.is_err() {
                        break;
                    }
                }
            });

            let transport =
                WebSocketRawTransport::external_streams_with(tcp_rx, Duration::from_secs(5), 32);

            // Bad clients: complete the WS upgrade then sit silent.
            // Without parallelism, accept() would serialize on these.
            let mut held = Vec::new();
            for _ in 0..5 {
                let (ws, _resp) =
                    tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
                        .await
                        .expect("rude client WS upgrade");
                held.push(ws);
            }

            // Healthy client: should be accepted promptly.
            let (_good_ws, _resp) =
                tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
                    .await
                    .expect("healthy client WS upgrade");

            let start = tokio::time::Instant::now();
            // Drain the worker output until we get a result. With
            // parallel handshakes, all 6 connect-side WS upgrades
            // complete promptly because each handshake runs in its
            // own task on the server.
            let result: SyncResult<_> =
                tokio::time::timeout(Duration::from_secs(3), transport.accept())
                    .await
                    .expect("accept should complete quickly");
            assert!(result.is_ok(), "accept returned err: {:?}", result.err());
            let elapsed = start.elapsed();
            assert!(
                elapsed < Duration::from_secs(2),
                "accept took {elapsed:?} — was the WS handshake serialized?"
            );

            drop(held);
        })
        .await;
}
