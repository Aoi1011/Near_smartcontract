use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::{stream::FusedStream, Future, SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::types::{PriceIdInput, RpcPriceFeed};

const PING_TIMEOUT_DURATION: Duration = Duration::from_secs(33); // 30s + 3s for delays

/// This class wraps websocket to provide a resilient web socket client.
///
/// It will reconnect if connection fails with exponential backoff. Also, in node, it will reconnect
/// if it receives no ping request from server within a while as indication of timeout (assuming
/// the server sends it regularly).
///
/// This class also logs events if logger is given and by replacing onError method you can handle
/// connection errors yourself (e.g: do not retry and close the connection).
pub struct ResilientWebSocket<F>
where
    F: FnMut(RpcPriceFeed) + Send + Sync,
{
    endpoint: String,
    ws_client: Option<Arc<TokioMutex<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
    ws_user_closed: bool,
    ws_failed_attempts: u32,
    ping_timeout: Option<Instant>,
    price_feed_callbacks: Arc<TokioMutex<HashMap<String, Vec<Arc<Mutex<F>>>>>>,
}

impl<F> ResilientWebSocket<F>
where
    F: FnMut(RpcPriceFeed) + Send + Sync + 'static,
{
    pub fn new(endpoint: &str, price_feed_callbacks: &HashMap<String, Vec<Arc<Mutex<F>>>>) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            ws_client: None,
            ws_user_closed: true,
            ws_failed_attempts: 0,
            ping_timeout: None,
            price_feed_callbacks: Arc::new(TokioMutex::new(price_feed_callbacks.clone())),
        }
    }

    pub async fn send(&mut self, data: Message) {
        log::info!("Sending {}", data.to_string());

        // self.wait_for_maybe_ready_websocket().await;

        if let Some(ws_client) = &self.ws_client {
            let ws_client = ws_client.clone();
            let mut ws_stream = ws_client.lock().await;

            match ws_stream.send(data).await {
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

    pub async fn start_web_socket(&mut self, tx: Option<tokio::sync::oneshot::Sender<()>>) {
        match self.ws_client {
            Some(_) => {
                return;
            }
            None => {
                log::info!("Creating Web Socket client");

                // let ws_client = self.ws_client.clone();
                // let mut client = ws_client.lock().await;
                match connect_async(&self.endpoint).await {
                    Ok((ws_stream, _)) => {
                        self.ws_client = Some(Arc::new(TokioMutex::new(ws_stream)));
                        self.ws_user_closed = false;
                        self.ws_failed_attempts = 0;
                        if let Some(tx) = tx {
                            let _ = tx.send(());
                        }
                        self.listen().await
                    }
                    Err(e) => {
                        log::error!("Websocket connection failed: {e}");
                    }
                }
            }
        }
    }

    async fn listen(&mut self) {
        let price_feed_callbacks = self.price_feed_callbacks.clone();

        if let Some(ws_client) = &self.ws_client {
            let ws_client = ws_client.clone();

            tokio::spawn(async move {
                let mut ws_stream = ws_client.lock().await;

                while let Some(message) = ws_stream.next().await {
                    match message {
                        Ok(Message::Text(text)) => {
                            on_message(text, &price_feed_callbacks).await;
                        }
                        Ok(Message::Ping(_)) => {
                            ws_stream.send(Message::Pong(vec![])).await.unwrap();
                        }
                        Ok(Message::Pong(_)) => {
                            // sender.send(Message::Pong(vec![])).await.unwrap();
                        }
                        Ok(Message::Close(_)) => {
                            // self.handle_close().await;
                            break;
                        }
                        Ok(Message::Binary(_)) | Ok(Message::Frame(_)) => {
                            unimplemented!()
                        }
                        Err(e) => {
                            on_error(e.to_string());
                            // self.handle_close().await;
                            break;
                        }
                    }
                }
            });
        }
    }

    /// Heartbeat is only enabled in node clients because they support handling
    /// ping-pong events.
    ///
    /// This approach only works when server constantly pings the clients which.
    /// Otherwise you might consider sending ping and acting on pong responses
    /// yourself.
    // async fn heartbeat(&mut self) {
    //     log::info!("Heartbeat");
    //     let mut ping_interval = tokio::time::interval(PING_TIMEOUT_DURATION);

    //     loop {
    //         ping_interval.tick().await;
    //         if let Some(ref mut client) = self.ws_client {
    //             if let Err(_) =
    //                 tokio::time::timeout(PING_TIMEOUT_DURATION, client.send(Message::Ping(vec![])))
    //                     .await
    //             {
    //                 log::warn!("Connection timed out!. Reconnecting...");
    //                 let _ = self.restart_unexpected_closed_web_socket().await;
    //             }
    //         }
    //     }
    // }

    async fn wait_for_maybe_ready_websocket(&mut self) {
        let mut waited_time = Duration::from_millis(0);

        // let ws_client = self.ws_client.clone();
        if let Some(ws_client) = &self.ws_client {
            let mut stream = ws_client.lock().await;
            log::info!("wait_for_maybe_ready: After locking ws_client");

            if !stream.is_terminated() {
                stream.close(None).await.unwrap();
                return;
            }

            if waited_time > Duration::from_secs(5) {
                stream.close(None).await.unwrap();
                return;
            } else {
                waited_time += Duration::from_millis(10);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    async fn handle_close(&mut self) {
        self.ws_client = None;
        if !self.ws_user_closed {
            self.ws_failed_attempts += 1;
            let wait_time = expo_backoff(self.ws_failed_attempts);
            tokio::time::sleep(wait_time).await;
            self.restart_unexpected_closed_web_socket().await;
        }
    }

    async fn restart_unexpected_closed_web_socket<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(async move {
            if self.ws_user_closed {
                return;
            }

            self.start_web_socket(None).await;
            self.wait_for_maybe_ready_websocket().await;

            // let ws_client = self.ws_client.clone();
            // let ws_stream = ws_client.lock().await;
            if self.ws_client.is_none() {
                log::error!("Couldn't recennect to websocket");
            } else {
                log::info!("Reconnected to websocket.");
            }
        })
    }

    pub async fn close_web_socket(&mut self) {
        if let Some(ws_client) = &self.ws_client {
            let ws_client = ws_client.clone();
            let mut ws_stream = ws_client.lock().await;

            ws_stream.close(None).await.unwrap();
            self.ws_client = None;
        }

        self.ws_user_closed = true;
    }
}

async fn on_message<F>(
    data: String,
    price_feed_callbacks: &Arc<TokioMutex<HashMap<String, Vec<Arc<Mutex<F>>>>>>,
) -> Result<(), String>
where
    F: FnMut(RpcPriceFeed) + Send + Sync,
{
    log::info!("Received message {}", data);

    let message: ServerMessage = serde_json::from_str(&data).map_err(|e| {
        log::error!("Error parsing message {data} as JSON");
        log::error!("{e}");
        on_error(e.to_string());
        "".to_string()
    })?;

    match message {
        ServerMessage::Response(response) => {
            if let ServerResponseMessage::Err { error } = response {
                log::error!("Error response from the websocket server {error}");
                on_error(error);
            }
        }
        ServerMessage::PriceUpdate { price_feed } => {
            let id = String::from_utf8(price_feed.id.0.to_vec()).unwrap();
            let price_feed_callbacks = price_feed_callbacks.lock().await;
            match price_feed_callbacks.get(&id) {
                Some(callbacks) => {
                    for callback in callbacks {
                        // let mut callback = callback.borrow_mut();
                        let mut callback = callback.lock().unwrap();
                        callback(price_feed.clone());
                    }
                }
                None => {
                    log::warn!("Ignoring unsupported server response {data}");
                }
            }
        }
    }

    Ok(())
}

fn on_error(error: String) {
    log::error!("{error}");
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
enum ClientMessage {
    #[serde(rename = "subscribe")]
    Subscribe {
        ids: Vec<PriceIdInput>,
        #[serde(default)]
        verbose: bool,
        #[serde(default)]
        binary: bool,
        #[serde(default)]
        allow_out_of_order: bool,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { ids: Vec<PriceIdInput> },
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
enum ServerMessage {
    #[serde(rename = "response")]
    Response(ServerResponseMessage),
    #[serde(rename = "price_update")]
    PriceUpdate { price_feed: RpcPriceFeed },
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "status")]
enum ServerResponseMessage {
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "error")]
    Err { error: String },
}

fn expo_backoff(attempts: u32) -> Duration {
    Duration::from_millis(2_u64.pow(attempts) * 100)
}
