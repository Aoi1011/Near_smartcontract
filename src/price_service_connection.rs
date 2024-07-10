use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use chrono::{DateTime, Timelike, Utc};
use pyth_sdk::PriceFeed;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::resilient_web_socket::ResilientWebSocket;

pub type PriceFeedUpdateCallback = Box<dyn Fn(PriceFeed) + Send + Sync>;

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

enum ClientMessageType {
    Subscribe,
    Unsubscribe,
}

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
    publish_time: u64,
    vaa: String,
}

pub struct PriceServiceConnection {
    http_client: Client,
    price_feed_callbacks: HashMap<String, HashSet<PriceFeedUpdateCallback>>,
    ws_client: Option<ResilientWebSocket>,
    ws_endpoint: String,
    price_feed_request_config: PriceFeedRequestConfig,
}

impl PriceServiceConnection {
    pub fn new(endpoint: &str, config: Option<PriceServiceConnectionConfig>) -> Self {
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

        Self {
            http_client: Client::new(),
            price_feed_callbacks: HashMap::new(),
            ws_client: None,
            ws_endpoint: endpoint.to_string(),
            price_feed_request_config,
        }
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

        let mut params = HashMap::new();
        for price_id in price_ids {
            params.insert("ids[]", price_id.to_string());
        }
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

        let url = format!("{}/api/latest_price_feeds", self.ws_endpoint);
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
        params.insert("publish_time", publish_time.second().to_string());

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
    pub async fn get_price_feed_ids(
        &self,
        price_ids: &[&str],
        publish_time: DateTime<Utc>,
    ) -> Vec<PriceFeed> {
        let mut params = HashMap::new();
        params.insert("ids", price_ids.join(","));
        params.insert("publish_time", publish_time.second().to_string());

        let url = format!("{}/api/price_feed_ids", self.ws_endpoint);
        let response = self
            .http_client
            .get(url)
            .query(&params)
            .send()
            .await
            .expect("Send request");

        let price_feed_json = response
            .json::<Vec<PriceFeed>>()
            .await
            .expect("deserializing");

        price_feed_json
    }

    pub async fn subscribe_price_feed_updates(&self, price_ids: &[&str], cb: String) {
        // if self.ws_client
    }
}
