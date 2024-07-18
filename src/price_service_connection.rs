use std::{collections::HashMap, rc::Rc, time::Duration};

use chrono::{DateTime, Timelike, Utc};
use pyth_sdk::PriceFeed;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

use crate::resilient_web_socket::ResilientWebSocket;

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
    http_retries: Option<u8>,

    /// Deprecated: please use priceFeedRequestConfig.verbose instead
    verbose: Option<bool>,

    /// Configuration for the price feed requests
    price_feed_request_config: Option<PriceFeedRequestConfig>,
}

#[derive(Serialize)]
enum ClientMessageType {
    Subscribe,
    Unsubscribe,
}

#[derive(Serialize)]
struct ClientMessage {
    r#type: ClientMessageType,
    ids: Vec<String>,
    verbose: Option<bool>,
    binary: Option<bool>,
    allow_out_of_order: Option<bool>,
}

enum ServerResponseStatus {
    Success,
    Error,
}

struct ServerResponse {
    r#type: String,
    status: ServerResponseStatus,
    error: Option<String>,
}

struct ServerPriceUpdate {
    r#type: String,
    price_feed: String,
}

enum ServerMessage {
    ServerResponse,
    ServerPriceUpdate,
}

#[derive(Debug, Deserialize)]
pub struct VaaResponse {
    #[serde(rename = "publishTime")]
    publish_time: i64,
    vaa: String,
}

pub struct PriceServiceConnection<F>
where
    F: FnMut(PriceFeed) + Send + Sync,
{
    http_client: Client,
    base_url: Url,
    price_feed_callbacks: HashMap<String, Vec<Rc<F>>>,
    ws_client: Option<ResilientWebSocket>,
    ws_endpoint: String,
    price_feed_request_config: PriceFeedRequestConfig,
}

impl<F> PriceServiceConnection<F>
where
    F: FnMut(PriceFeed) + Send + Sync,
{
    pub fn new(
        endpoint: &str,
        config: Option<PriceServiceConnectionConfig>,
    ) -> Result<Self, crate::error::PriceServiceError> {
        let price_feed_request_config = if let Some(price_service_config) = config {
            if let Some(config) = price_service_config.price_feed_request_config {
                let verbose = match config.verbose {
                    Some(config_verbose) => Some(config_verbose),
                    None => price_service_config.verbose,
                };

                PriceFeedRequestConfig {
                    binary: config.binary,
                    verbose,
                    allow_out_of_order: config.allow_out_of_order,
                }
            } else {
                PriceFeedRequestConfig {
                    binary: None,
                    verbose: price_service_config.verbose,
                    allow_out_of_order: None,
                }
            }
        } else {
            PriceFeedRequestConfig {
                binary: None,
                verbose: None,
                allow_out_of_order: None,
            }
        };

        Ok(Self {
            http_client: Client::new(),
            base_url: Url::parse(endpoint)?,
            price_feed_callbacks: HashMap::new(),
            ws_client: None,
            ws_endpoint: endpoint.to_string(),
            price_feed_request_config,
        })
    }

    /// Fetch Latest PriceFeeds of given price ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price ids)
    pub async fn get_latest_price_feeds(
        &self,
        price_ids: &[&str],
    ) -> Result<Vec<PriceFeed>, reqwest::Error> {
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

        let url = self.base_url.join("/api/latest_price_feeds");
        let response = self.http_client.get(url).query(&params).send().await?;

        let price_feed_json = response.json::<Vec<PriceFeed>>().await?;

        Ok(price_feed_json)
    }

    /// Fetch latest VAA of given price ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price ids)
    ///
    /// This function is coupled to wormhole implemntation.
    pub async fn get_latest_vass(&self, price_ids: &[&str]) -> Result<Vec<String>, reqwest::Error> {
        if price_ids.is_empty() {
            return Ok(vec![]);
        }

        let mut params = HashMap::new();
        for price_id in price_ids {
            params.insert("ids[]", price_id.to_string());
        }

        let url = format!("{}/api/latest_vaas", self.ws_endpoint);
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
    ) -> Result<VaaResponse, String> {
        let mut params = HashMap::new();
        params.insert("id", price_id.to_string());
        params.insert("publish_time", publish_time.timestamp().to_string());

        let url = format!("{}/api/get_vaa", self.ws_endpoint);
        let response = self
            .http_client
            .get(url)
            .query(&params)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match response.status() {
            StatusCode::OK => {
                let vaa = response
                    .json::<VaaResponse>()
                    .await
                    .map_err(|e| e.to_string())?;

                Ok(vaa)
            }
            status => {
                let err_str = response.json::<String>().await.map_err(|e| {
                    format!("Error status: {status}, Error message: {}", e.to_string())
                })?;

                Err(err_str)
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
    ) -> Result<PriceFeed, String> {
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

        let url = format!("{}/api/get_price_feed", self.ws_endpoint);
        let response = self
            .http_client
            .get(url)
            .query(&params)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match response.status() {
            StatusCode::OK => {
                let price_feed_json = response
                    .json::<PriceFeed>()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(price_feed_json)
            }
            status => {
                let err_str = response.json::<String>().await.map_err(|e| {
                    format!("Error status: {status}, Error message: {}", e.to_string())
                })?;

                Err(err_str)
            }
        }
    }

    /// Fetch the list of available price feed ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response.
    pub async fn get_price_feed_ids(&self) -> Vec<String> {
        let url = format!("{}/api/price_feed_ids", self.ws_endpoint);
        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .expect("Send request");

        let price_feed_json = response.json::<Vec<String>>().await.expect("deserializing");

        price_feed_json
    }

    /// Subscribe to updates for given price ids.
    ///
    /// It will close the websocket connection if it's not subscribed to any price feed updates anymore.
    /// Also, it won't throw any exception if given price ids are invalid or connection errors. Instead,
    /// it calls `connection.onWsError`. If you want to handle the errors you should set the
    /// `onWsError` function to your custom error handler.
    pub async fn subscribe_price_feed_updates(&mut self, price_ids: &[&str], cb: F) {
        if self.ws_client.is_none() {
            self.start_web_socket().await;
        }

        let price_ids: Vec<&str> = price_ids
            .iter()
            .map(|price_id| {
                if price_id.starts_with("0x") {
                    &price_id[2..]
                } else {
                    price_id
                }
            })
            .collect();

        let mut new_price_ids = Vec::new();

        let callback = Rc::new(cb);
        for id in price_ids {
            if !self.price_feed_callbacks.contains_key(id) {
                self.price_feed_callbacks.insert(id.to_string(), Vec::new());
                new_price_ids.push(id.to_string());
            }

            let price_feed_callbacks = self.price_feed_callbacks.get_mut(id).unwrap();
            price_feed_callbacks.push(callback.clone());
        }

        let message = ClientMessage {
            ids: new_price_ids,
            r#type: ClientMessageType::Subscribe,
            verbose: self.price_feed_request_config.verbose,
            binary: self.price_feed_request_config.binary,
            allow_out_of_order: self.price_feed_request_config.allow_out_of_order,
        };

        if let Some(ref mut ws_client) = self.ws_client {
            ws_client
                .send(Message::Text(serde_json::to_string(&message).unwrap()))
                .await;
        }
    }

    /// Starts connection websocket.
    ///
    /// This function is called automatically upon subscribing to price feed updates.
    pub async fn start_web_socket(&mut self) {
        let endpoint = self.ws_endpoint.to_string();
        let endpoint = endpoint.replace("https", "wss");
        let mut web_socket = ResilientWebSocket::new(&endpoint);
        web_socket.start_web_socket().await;

        self.ws_client = Some(web_socket);
    }

    /// Closes connection websocket.
    ///
    /// At termination, the websocket should be closed to finish the
    /// process elegantly. It will automatically close when the connection
    /// is subscribed to no price feeds.
    ///
    pub async fn close_web_socket(&mut self) {
        if let Some(ref mut client) = self.ws_client {
            client.close_web_socket().await;
            self.ws_client = None;
            self.price_feed_callbacks.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    const ENDPOINT: &str = "https://hermes.pyth.network";

    type Cb = fn(PriceFeed);

    #[tokio::test]
    async fn test_http_endpoints() {
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, None).expect("Failed to construct");

        let ids = connection.get_price_feed_ids().await;
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
            http_retries: None,
            verbose: Some(true),
            price_feed_request_config: None,
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection.get_price_feed_ids().await;
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
            http_retries: None,
            verbose: None,
            price_feed_request_config: Some(price_feed_request_config),
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection.get_price_feed_ids().await;
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
            http_retries: None,
            verbose: None,
            price_feed_request_config: Some(price_feed_request_config),
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection.get_price_feed_ids().await;
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
            http_retries: None,
            verbose: None,
            price_feed_request_config: Some(price_feed_request_config),
        };
        let connection: PriceServiceConnection<Cb> =
            PriceServiceConnection::new(ENDPOINT, Some(price_service_connection_config))
                .expect("Failed to construct");

        let ids = connection.get_price_feed_ids().await;
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

        let ids = connection.get_price_feed_ids().await;
        assert!(!ids.is_empty());

        let mut counter = HashMap::new();
        let mut total_counter = 0;

        let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();
        connection
            .subscribe_price_feed_updates(&price_ids, |price_feed| {
                assert!(price_feed.get_metadata().is_some());
                assert!(price_feed.get_vaa().is_some());

                counter
                    .entry(price_feed.id.to_string())
                    .and_modify(|counter| *counter += 1)
                    .or_insert(1);
                total_counter += 1;
            })
            .await;

        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        connection.close_web_socket().await;

        assert_eq!(total_counter, 30);

        for id in ids {
            assert!(counter.get(&id).is_some());
        }
    }
}
