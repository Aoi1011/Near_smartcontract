use std::time::{Duration, Instant};

use futures_util::{stream::FusedStream, SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

/// This class wraps websocket to provide a resilient web socket client.
///
/// It will reconnect if connection fails with exponential backoff. Also, in node, it will reconnect
/// if it receives no ping request from server within a while as indication of timeout (assuming
/// the server sends it regularly).
///
/// This class also logs events if logger is given and by replacing onError method you can handle
/// connection errors yourself (e.g: do not retry and close the connection).
pub struct ResilientWebSocket {
    endpoint: String,
    ws_client: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    ws_user_closed: bool,
    ws_failed_attempts: usize,
    ping_timeout: Option<Instant>,
}

impl ResilientWebSocket {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            ws_client: None,
            ws_user_closed: true,
            ws_failed_attempts: 0,
            ping_timeout: None,
        }
    }

    async fn send(&mut self, data: Message) {
        log::info!("Sending {}", data.to_string());

        self.wait_for_maybe_ready_websocket().await;

        match self.ws_client {
            Some(ref mut client) => {
                let (mut write, _read) = client.split();
                write.send(data).await.expect("Failed to send message");
            }
            None => {
                log::error!("Couldn't connect to the websocket server. Error callback is called.");
            }
        }
    }

    async fn start_web_socket(&mut self) {
        if let Some(ref mut client) = self.ws_client {
            log::info!("Creating Web Socket client");

            let (ws_stream, _) = connect_async(&self.endpoint)
                .await
                .expect("Failed to connect");
            self.ws_user_closed = false;
        }
    }

    async fn wait_for_maybe_ready_websocket(&mut self) {
        let mut waited_time = 0;

        if let Some(ref mut client) = self.ws_client {
            while !client.is_terminated() {
                if waited_time > 5000 {
                    client.close(None).await;
                } else {
                    waited_time += 10;
                    tokio::time::sleep(Duration::from_millis(10));
                }
            }
        }
    }
}
