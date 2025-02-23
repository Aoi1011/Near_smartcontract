use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Timelike, Utc};
use pyth_sdk::PriceFeed;
use reqwest::{Client, ClientBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::tungstenite::Message;

use crate::{
    error::PriceServiceError,
    resilient_web_socket::ResilientWebSocket,
    types::{PriceIdInput, RpcPriceFeed},
};

#[derive(Debug, Default)]
pub struct PriceFeedRequestConfig {
    /// Optional verbose to request for verbose information from the service
    verbose: Option<bool>,

    /// Optional binary to include the price feeds binary update data
    binary: Option<bool>,

    /// Optional config for the websocket subscription to receive out of order updates
    allow_out_of_order: Option<bool>,
}

#[derive(Debug, Default)]
pub struct PriceServiceConnectionConfig {
    /// Timeout of each request (for all of retries). Default: 5000ms
    timeout: Option<Duration>,

    /// Number of times a HTTP request will be retried before the API returns a failure. Default: 3.
    ///
    /// The connection uses exponential back-off for the delay between retries. However,
    /// it will timeout regardless of the retries at the configured `timeout` time.
    ///
    /// In the future, will use retry: https://github.com/seanmonstar/reqwest/issues/799
    // http_retries: Option<u8>,

    /// Deprecated: please use priceFeedRequestConfig.verbose instead
    verbose: Option<bool>,

    /// Configuration for the price feed requests
    price_feed_request_config: Option<PriceFeedRequestConfig>,
}

#[derive(Debug, Deserialize)]
pub struct VaaResponse {
    #[serde(rename = "publishTime")]
    pub publish_time: i64,
    pub vaa: String,
}

pub struct PriceServiceConnection<F>
where
    F: FnMut(RpcPriceFeed) + Send + Sync,
{
    http_client: Client,
    base_url: Url,
    price_feed_callbacks: HashMap<String, Vec<Arc<Mutex<F>>>>,
    ws_client: Option<Arc<TokioMutex<ResilientWebSocket<F>>>>,
    ws_endpoint: Url,
    price_feed_request_config: PriceFeedRequestConfig,
}

impl<F> PriceServiceConnection<F>
where
    F: FnMut(RpcPriceFeed) + Send + Sync + 'static,
{
    pub fn new(
        endpoint: &str,
        config: Option<PriceServiceConnectionConfig>,
    ) -> Result<Self, PriceServiceError> {
        let price_feed_request_config: PriceFeedRequestConfig;
        let timeout: Duration;

        match config {
            Some(ref price_service_config) => {
                if let Some(ref config) = price_service_config.price_feed_request_config {
                    let verbose = match config.verbose {
                        Some(config_verbose) => Some(config_verbose),
                        None => price_service_config.verbose,
                    };

                    price_feed_request_config = PriceFeedRequestConfig {
                        binary: config.binary,
                        verbose,
                        allow_out_of_order: config.allow_out_of_order,
                    }
                } else {
                    price_feed_request_config = PriceFeedRequestConfig {
                        binary: None,
                        verbose: price_service_config.verbose,
                        allow_out_of_order: None,
                    }
                }

                timeout = price_service_config
                    .timeout
                    .unwrap_or(Duration::from_millis(5000));
            }
            None => {
                price_feed_request_config = PriceFeedRequestConfig {
                    binary: None,
                    verbose: None,
                    allow_out_of_order: None,
                };

                timeout = Duration::from_millis(5000);
            }
        };

        let base_url = Url::parse(endpoint)?;

        let mut ws_endpoint = base_url.clone();
        match base_url.scheme() {
            "http" => ws_endpoint.set_scheme("ws").unwrap(),
            "https" => ws_endpoint.set_scheme("wss").unwrap(),
            _ => {
                return Err(PriceServiceError::BadUrl(
                    url::ParseError::InvalidIpv4Address,
                ))
            }
        };

        let http_client = ClientBuilder::new().timeout(timeout).build()?;

        Ok(Self {
            http_client,
            base_url,
            price_feed_callbacks: HashMap::new(),
            ws_client: None,
            ws_endpoint,
            price_feed_request_config,
        })
    }

    /// Fetch Latest PriceFeeds of given price ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price ids)
    pub async fn get_latest_price_feeds(
        &self,
        price_ids: &[&str],
    ) -> Result<Vec<PriceFeed>, PriceServiceError> {
        if price_ids.is_empty() {
            return Ok(vec![]);
        }

        let mut params = Vec::new();
        for price_id in price_ids {
            params.push(("ids[]", price_id.to_string()));
        }
        let verbose = match self.price_feed_request_config.verbose {
            Some(verbose) => verbose,
            None => true,
        };
        params.push(("verbose", verbose.to_string()));

        let binary = match self.price_feed_request_config.binary {
            Some(binary) => binary,
            None => true,
        };
        params.push(("binary", binary.to_string()));

        let url = match self.base_url.join("/api/latest_price_feeds") {
            Ok(url) => url,
            Err(e) => return Err(PriceServiceError::BadUrl(e)),
        };
        let response = self.http_client.get(url).query(&params).send().await?;
        let price_feed_json = response.json::<Vec<PriceFeed>>().await?;

        Ok(price_feed_json)
    }

    /// Fetch latest VAA of given price ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price ids)
    ///
    /// This function is coupled to wormhole implemntation.
    pub async fn get_latest_vass(
        &self,
        price_ids: &[&str],
    ) -> Result<Vec<String>, PriceServiceError> {
        if price_ids.is_empty() {
            return Ok(vec![]);
        }

        let mut params = HashMap::new();
        for price_id in price_ids {
            params.insert("ids[]", price_id.to_string());
        }

        let url = match self.base_url.join("/api/latest_vaas") {
            Ok(url) => url,
            Err(e) => return Err(PriceServiceError::BadUrl(e)),
        };
        let response = self.http_client.get(url).query(&params).send().await?;

        let vaas = response.json::<Vec<String>>().await?;
        Ok(vaas)
    }

    /// Fetch the earliest VAA of the given price id that is published since the given publish time.
    /// This will throw an error if the given publish time is in the future, or if the publish time
    /// is old and the price service endpoint does not have a db backend for historical requests.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price id)
    ///
    /// This function is coupled to wormhole implemntation.
    pub async fn get_vaa(
        &self,
        price_id: &str,
        publish_time: DateTime<Utc>,
    ) -> Result<VaaResponse, PriceServiceError> {
        let mut params = HashMap::new();
        params.insert("id", price_id.to_string());
        params.insert("publish_time", publish_time.timestamp().to_string());

        let url = match self.base_url.join("/api/get_vaa") {
            Ok(url) => url,
            Err(e) => return Err(PriceServiceError::BadUrl(e)),
        };
        let response = self.http_client.get(url).query(&params).send().await?;

        match response.status() {
            StatusCode::OK => {
                let vaa = response.json::<VaaResponse>().await?;

                Ok(vaa)
            }
            _status => {
                let err_str = response.json::<String>().await?;

                Err(PriceServiceError::NotJson(err_str))
            }
        }
    }

    /// Fetch the PriceFeed of the given price id that is published since the given publish time.
    /// This will throw an error if the given publish time is in the future, or if the publish time
    /// is old and the price service endpoint does not have a db backend for historical requests.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price id)
    pub async fn get_price_feed(
        &self,
        price_id: &str,
        publish_time: DateTime<Utc>,
    ) -> Result<PriceFeed, PriceServiceError> {
        let mut params = HashMap::new();
        params.insert("id", price_id.to_string());
        params.insert("publish_time", publish_time.second().to_string());

        let verbose = match self.price_feed_request_config.verbose {
            Some(verbose) => verbose,
            None => true,
        };
        params.insert("verbose", verbose.to_string());

        let binary = match self.price_feed_request_config.binary {
            Some(binary) => binary,
            None => true,
        };
        params.insert("binary", binary.to_string());

        let url = match self.base_url.join("/api/get_price_feed") {
            Ok(url) => url,
            Err(e) => return Err(PriceServiceError::BadUrl(e)),
        };
        let response = self.http_client.get(url).query(&params).send().await?;

        match response.status() {
            StatusCode::OK => {
                let price_feed_json = response.json::<PriceFeed>().await?;
                Ok(price_feed_json)
            }
            _status => {
                let err_str = response.json::<String>().await?;

                Err(PriceServiceError::NotJson(err_str))
            }
        }
    }

    /// Fetch the list of available price feed ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response.
    pub async fn get_price_feed_ids(&self) -> Result<Vec<String>, PriceServiceError> {
        let url = match self.base_url.join("/api/price_feed_ids") {
            Ok(url) => url,
            Err(e) => return Err(PriceServiceError::BadUrl(e)),
        };
        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .expect("Send request");

        let price_feed_json = response.json::<Vec<String>>().await?;

        Ok(price_feed_json)
    }

    /// Subscribe to updates for given price ids.
    ///
    /// It will close the websocket connection if it's not subscribed to any price feed updates anymore.
    /// Also, it won't throw any exception if given price ids are invalid or connection errors. Instead,
    /// it calls `connection.onWsError`. If you want to handle the errors you should set the
    /// `onWsError` function to your custom error handler.
    pub async fn subscribe_price_feed_updates(
        &mut self,
        price_ids: &[&str],
        cb: F,
    ) -> Result<(), PriceServiceError> {
        let mut new_price_ids = Vec::new();

        let callback = Arc::new(Mutex::new(cb));
        for id in price_ids.iter() {
            if !self.price_feed_callbacks.contains_key(*id) {
                self.price_feed_callbacks.insert(id.to_string(), Vec::new());

                let id_bytes = hex::decode(id).expect("Decoding failed");
                let id_input = id_bytes.try_into().expect("Incorrect length");
                new_price_ids.push(PriceIdInput(id_input));
            }

            let price_feed_callbacks = self.price_feed_callbacks.get_mut(*id).unwrap();
            price_feed_callbacks.push(callback.clone());
        }

        let message = ClientMessage::Subscribe {
            ids: new_price_ids,
            verbose: self.price_feed_request_config.verbose.unwrap_or(false),
            binary: self.price_feed_request_config.binary.unwrap_or(false),
            allow_out_of_order: self
                .price_feed_request_config
                .allow_out_of_order
                .unwrap_or(false),
        };

        if self.ws_client.is_none() {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let initial_message = serde_json::to_string(&message)
                .map_err(|e| PriceServiceError::NotJson(e.to_string()))?;

            self.start_web_socket(tx, initial_message).await;
            let _ = rx.await;
        }

        // log::info!("WS_Client: {}", self.ws_client.is_some());

        // if let Some(ref mut ws_client) = self.ws_client {
        //     let ws_client = ws_client.clone();
        //     let message = serde_json::to_string(&message)
        //         .map_err(|e| PriceServiceError::NotJson(e.to_string()))?;

        //     tokio::spawn(async move {
        //         log::info!("Before sending message");
        //         let mut ws_client = ws_client.lock().await;
        //         log::info!("Sending message");
        //         ws_client.send(Message::Text(message)).await;
        //     });
        // }

        Ok(())
    }

    /// Unsubscribe from updates for given price ids.
    ///
    /// It will close the websocket connection if it's not subscribed to any price feed updates anymore.
    /// Also, it won't throw any exception if given price ids are invalid or connection errors. Instead,
    /// it calls `connection.onWsError`. If you want to handle the errors you should set the
    /// `onWsError` function to your custom error handler.
    pub async fn unsubscribe_price_feed_updates(
        &mut self,
        price_ids: &[&str],
    ) -> Result<(), PriceServiceError> {
        let price_id_inputs: Vec<PriceIdInput> = price_ids
            .iter()
            .map(|id| {
                let id_bytes = hex::decode(id).expect("Decoding failed");
                let id_input = id_bytes.try_into().expect("Incorrect length");
                PriceIdInput(id_input)
            })
            .collect();

        let message = ClientMessage::Subscribe {
            ids: price_id_inputs.clone(),
            verbose: self.price_feed_request_config.verbose.unwrap_or(false),
            binary: self.price_feed_request_config.binary.unwrap_or(false),
            allow_out_of_order: self
                .price_feed_request_config
                .allow_out_of_order
                .unwrap_or(false),
        };

        if self.ws_client.is_none() {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let initial_message = serde_json::to_string(&message)
                .map_err(|e| PriceServiceError::NotJson(e.to_string()))?;

            self.start_web_socket(tx, initial_message).await;
            let _ = rx.await;
        }

        // let remove_price_ids = Vec::new();

        for id in price_ids {
            self.price_feed_callbacks.remove(&id.to_string());
        }

        if let Some(ref mut client) = self.ws_client {
            let mut client = client.lock().await;
            let client_message = ClientMessage::Unsubscribe {
                ids: price_id_inputs,
            };
            let message = serde_json::to_string(&client_message)
                .map_err(|e| PriceServiceError::NotJson(e.to_string()))?;
            client.send(Message::Text(message)).await;
        }

        if self.price_feed_callbacks.is_empty() {
            self.close_web_socket().await;
        }

        Ok(())
    }

    /// Starts connection websocket.
    ///
    /// This function is called automatically upon subscribing to price feed updates.
    pub async fn start_web_socket(
        &mut self,
        tx: tokio::sync::oneshot::Sender<()>,
        initial_message: String,
    ) {
        let endpoint = format!("{}ws", self.ws_endpoint);
        let web_socket = Arc::new(TokioMutex::new(ResilientWebSocket::new(
            &endpoint,
            &self.price_feed_callbacks,
        )));
        self.ws_client = Some(web_socket.clone());

        let w_socket_clone = web_socket.clone();

        tokio::spawn(async move {
            let mut ws = w_socket_clone.lock().await;
            ws.start_web_socket(Some(tx), initial_message).await;
        });
    }

    /// Closes connection websocket.
    ///
    /// At termination, the websocket should be closed to finish the
    /// process elegantly. It will automatically close when the connection
    /// is subscribed to no price feeds.
    ///
    pub async fn close_web_socket(&mut self) {
        if let Some(ref mut client) = self.ws_client {
            let ws_client = client.clone();
            let mut ws_client = ws_client.lock().await;
            ws_client.close_web_socket().await;
            self.ws_client = None;
            self.price_feed_callbacks.clear();
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
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

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use crate::types::RpcPriceIdentifier;

    use super::*;

    const ENDPOINT: &str = "https://hermes.pyth.network";

    type Cb = fn(RpcPriceFeed);

    #[tokio::test]
    async fn test_http_endpoints() {
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, None).expect("Failed to construct");

        let ids = connection
            .get_price_feed_ids()
            .await
            .expect("Failed to get price feed ids");
        assert!(!ids.is_empty());

        let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();
        let price_feeds = connection.get_latest_price_feeds(&price_ids).await;
        assert!(price_feeds.is_ok());

        let price_feeds = price_feeds.unwrap();
        assert_eq!(price_feeds.len(), 2);
    }

    #[tokio::test]
    async fn test_get_price_feed_with_verbose_flag_works() {
        let price_service_connection_config = PriceServiceConnectionConfig {
            timeout: None,
            verbose: Some(true),
            price_feed_request_config: None,
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection
            .get_price_feed_ids()
            .await
            .expect("Failed to get price feed ids");
        assert!(!ids.is_empty());

        let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();
        let price_feeds = connection.get_latest_price_feeds(&price_ids).await;
        assert!(price_feeds.is_ok());

        let price_feeds = price_feeds.unwrap();
        assert_eq!(price_feeds.len(), 2);
    }

    #[tokio::test]
    async fn test_get_price_feed_with_binary_flag_works() {
        let price_feed_request_config = PriceFeedRequestConfig {
            verbose: None,
            binary: Some(true),
            allow_out_of_order: None,
        };
        let price_service_connection_config = PriceServiceConnectionConfig {
            timeout: None,
            verbose: None,
            price_feed_request_config: Some(price_feed_request_config),
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection
            .get_price_feed_ids()
            .await
            .expect("Failed to get price feed ids");
        assert!(!ids.is_empty());

        let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();
        let price_feeds = connection.get_latest_price_feeds(&price_ids).await;
        assert!(price_feeds.is_ok());

        let price_feeds = price_feeds.unwrap();
        assert_eq!(price_feeds.len(), 2);
    }

    #[tokio::test]
    async fn test_get_latest_vaa_works() {
        let price_feed_request_config = PriceFeedRequestConfig {
            verbose: None,
            binary: Some(true),
            allow_out_of_order: None,
        };
        let price_service_connection_config = PriceServiceConnectionConfig {
            timeout: None,
            verbose: None,
            price_feed_request_config: Some(price_feed_request_config),
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection
            .get_price_feed_ids()
            .await
            .expect("Failed to get price feed ids");
        assert!(!ids.is_empty());

        let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();
        let vaas = connection
            .get_latest_vass(&price_ids)
            .await
            .expect("Failed to get latest vaas");
        assert!(!vaas.is_empty());
    }

    #[tokio::test]
    async fn test_get_vaa_works() {
        let price_feed_request_config = PriceFeedRequestConfig {
            verbose: None,
            binary: Some(true),
            allow_out_of_order: None,
        };
        let price_service_connection_config = PriceServiceConnectionConfig {
            timeout: None,
            verbose: None,
            price_feed_request_config: Some(price_feed_request_config),
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection
            .get_price_feed_ids()
            .await
            .expect("Failed to get latest vaas");
        assert!(!ids.is_empty());

        let publish_time_10_sec_ago = Utc::now() - Duration::seconds(10);
        let VaaResponse { publish_time, vaa } = connection
            .get_vaa(
                "19d75fde7fee50fe67753fdc825e583594eb2f51ae84e114a5246c4ab23aff4c",
                publish_time_10_sec_ago,
            )
            .await
            .expect("Failed to get latest vaas");
        assert!(!vaa.is_empty());
        assert!(publish_time >= publish_time_10_sec_ago.timestamp());
    }

    #[tokio::test]
    async fn test_websocket_subscription_works_without_verbose_and_binary() {
        let mut connection =
            PriceServiceConnection::new(ENDPOINT, None).expect("Failed to construct");

        let ids = connection
            .get_price_feed_ids()
            .await
            .expect("Failed to get latest vaas");
        assert!(!ids.is_empty());

        let counter = Arc::new(TokioMutex::new(HashMap::new()));
        let total_counter = Arc::new(TokioMutex::new(0));

        let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();

        let counter_clone = counter.clone();
        let total_counter_clone = total_counter.clone();

        connection
            .subscribe_price_feed_updates(&price_ids, move |price_feed| {
                assert!(price_feed.metadata.is_some());
                assert!(price_feed.vaa.is_some());

                let counter = counter_clone.clone();
                let total_counter = total_counter_clone.clone();

                tokio::spawn(async move {
                    let mut counter = counter.lock().await;
                    let mut total_counter = total_counter.lock().await;

                    counter
                        .entry(price_feed.id)
                        .and_modify(|counter| *counter += 1)
                        .or_insert(1);
                    *total_counter += 1;
                });
            })
            .await
            .expect("Failed to subscribe price feed updates");

        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        connection.close_web_socket().await;

        let total_counter = total_counter.lock().await;
        assert_eq!(*total_counter, 30);

        let counter = counter.lock().await;
        for id in ids {
            let id_bytes = hex::decode(id).expect("Decoding failed");
            let id_input = id_bytes.try_into().expect("Incorrect length");
            assert!(counter.get(&RpcPriceIdentifier(id_input)).is_some());
        }
    }
}
