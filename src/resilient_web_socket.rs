use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use futures_util::{stream::FusedStream, Future, SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

const PING_TIMEOUT_DURATION: Duration = Duration::from_secs(33); // 30s + 3s for delays

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

    pub async fn send(&mut self, data: Message) {
        log::info!("Sending {}", data.to_string());

        self.wait_for_maybe_ready_websocket().await;

        if let Some(ref mut client) = self.ws_client {
            let (mut write, _read) = client.split();
            match write.send(data).await {
                Ok(_) => {
                    log::info!("Sent");
                }
                Err(e) => {
                    log::error!("Error sending message: {e}");
                }
            }
        } else {
            log::error!("Couldn't connect to the websocket server. Error callback is called.");
        }
    }

    pub async fn start_web_socket(&mut self) {
        if self.ws_client.is_some() {
            return;
        }
        log::info!("Creating Web Socket client");

        match connect_async(&self.endpoint).await {
            Ok((ws_stream, _)) => {
                self.ws_client = Some(ws_stream);
                self.ws_user_closed = false;
                self.ws_failed_attempts = 0;
                self.heartbeat().await;
            }
            Err(e) => {
                log::error!("Websocket connection failed: {e}");
            }
        }
    }

    /// Heartbeat is only enabled in node clients because they support handling
    /// ping-pong events.
    ///
    /// This approach only works when server constantly pings the clients which.
    /// Otherwise you might consider sending ping and acting on pong responses
    /// yourself.
    async fn heartbeat(&mut self) {
        log::info!("Heartbeat");
        let mut ping_interval = tokio::time::interval(PING_TIMEOUT_DURATION);

        loop {
            ping_interval.tick().await;
            if let Some(ref mut client) = self.ws_client {
                if let Err(_) =
                    tokio::time::timeout(PING_TIMEOUT_DURATION, client.send(Message::Ping(vec![])))
                        .await
                {
                    log::warn!("Connection timed out!. Reconnecting...");
                    let _ = self.restart_unexpected_closed_web_socket().await;
                }
            }
        }
    }

    async fn wait_for_maybe_ready_websocket(&mut self) {
        let mut waited_time = Duration::from_millis(0);

        while let Some(ref mut client) = self.ws_client {
            if !client.is_terminated() {
                client.close(None).await.unwrap();
                return;
            }

            if waited_time > Duration::from_secs(5) {
                client.close(None).await.unwrap();
                return;
            } else {
                waited_time += Duration::from_millis(10);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    async fn restart_unexpected_closed_web_socket<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(async move {
            if self.ws_user_closed {
                return;
            }

            self.start_web_socket().await;
            self.wait_for_maybe_ready_websocket().await;

            if self.ws_client.is_none() {
                log::error!("Couldn't recennect to websocket");
            } else {
                log::info!("Reconnected to websocket.");
            }
        })
    }

    pub async fn close_web_socket(&mut self) {
        if let Some(ref mut client) = self.ws_client {
            client.close(None).await.unwrap();
            self.ws_client = None;
        }

        self.ws_user_closed = true;
    }
}

fn expo_backoff(attempts: u32) -> Duration {
    Duration::from_millis(2_u64.pow(attempts) * 100)
}
