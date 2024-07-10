use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use chrono::{DateTime, Timelike, Utc};
use pyth_sdk::PriceFeed;
use reqwest::Client;
use serde::Deserialize;

use crate::resilient_web_socket::ResilientWebSocket;

pub type PriceFeedUpdateCallback = Box<dyn Fn(PriceFeed) + Send + Sync>;

#[derive(Debug, Default, Deserialize)]
enum Encoding {
    #[default]
    Hex,

    Base64,
}

impl ToString for Encoding {
    fn to_string(&self) -> String {
        match self {
            Encoding::Hex => String::from("hex"),
            Encoding::Base64 => String::from("base64"),
        }
    }
}

#[derive(Debug, Default)]
pub struct PriceFeedRequestConfig {
    /// Optional encoding type.
    /// If true, return the price update in the encoding specified by the encoding parameter.
    /// Default is hex
    encoding: Encoding,

    /// If true, include the parsed price update in the parsed field of each returned feed.
    /// Default is true
    parsed: bool,
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

#[derive(Default, Deserialize)]
struct Binary {
    encoding: Encoding,
    data: Vec<String>,
}

#[derive(Deserialize)]
struct ServerResponse {
    #[serde(skip_deserializing)]
    binary: Binary,

    #[serde(skip_deserializing)]
    parsed: Vec<PriceFeed>,
}

struct ServerPriceUpdate {
    r#type: String,
    price_feed: String,
}

enum ServerMessage {
    ServerResponse,
    ServerPriceUpdate,
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
        let default = PriceFeedRequestConfig {
            encoding: Encoding::default(),
            parsed: true,
        };

        let price_feed_request_config = if let Some(config) = config {
            config.price_feed_request_config.unwrap_or(default)
        } else {
            default
        };
        // let price_feed_request_config = if let Some(price_service_config) = config {
        //     if let Some(config) = price_service_config.price_feed_request_config {
        //         let verbose = match config.verbose {
        //             Some(config_verbose) => Some(config_verbose),
        //             None => price_service_config.verbose,
        //         };

        //         PriceFeedRequestConfig {
        //             binary: config.binary,
        //             verbose,
        //             allow_out_of_order: config.allow_out_of_order,
        //         }
        //     } else {
        //         PriceFeedRequestConfig {
        //             binary: None,
        //             verbose: price_service_config.verbose,
        //             allow_out_of_order: None,
        //         }
        //     }
        // } else {
        //     PriceFeedRequestConfig {
        //         encoding: None,
        //         parsed: None,
        //     }
        // };

        Self {
            http_client: Client::new(),
            price_feed_callbacks: HashMap::new(),
            ws_client: None,
            ws_endpoint: endpoint.to_string(),
            price_feed_request_config,
        }
    }

    /// Get the latest price updates by price feed id.
    ///
    /// Given a collection of price feed ids, retrieve the latest Pyth price for each price feed.
    ///
    /// Fetch Latest PriceFeeds of given price ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price ids)
    pub async fn get_latest_price_feeds(&self, price_ids: &[&str]) -> Vec<PriceFeed> {
        if price_ids.is_empty() {
            return vec![];
        }

        let mut params = HashMap::new();
        params.insert("ids[]", price_ids.join(","));
        // params.insert(
        //     "encoding",
        //     self.price_feed_request_config.encoding.to_string(),
        // );
        // params.insert("parsed", self.price_feed_request_config.parsed.to_string());

        params.insert("verbose", true.to_string());
        params.insert("binary", true.to_string());
        let url = format!("{}/api/latest_price_feeds", self.ws_endpoint);
        let response = self
            .http_client
            .get(url)
            .query(&params)
            .send()
            .await
            .expect("Send request");

        println!("{:?}", response.url());
        println!("{:?}", response.text().await);

        todo!()

        // match response.json::<ServerResponse>().await {
        //     Ok(response) => {
        //         return response.parsed;
        //     }
        //     Err(e) => {
        //         println!("Failed to deserialize: {e}");
        //         vec![]
        //     }
        // }
    }

    /// Fetch latest VAA of given price ids.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price ids)
    ///
    /// This function is coupled to wormhole implemntation.
    pub async fn get_latest_vass(price_ids: &[&str]) {}

    /// Fetch the earliest VAA of the given price id that is published since the given publish time.
    /// This will throw an error if the given publish time is in the future, or if the publish time
    /// is old and the price service endpoint does not have a db backend for historical requests.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price id)
    ///
    /// This function is coupled to wormhole implemntation.
    pub async fn get_vaa(&self, price_ids: &[&str], publish_time: DateTime<Utc>) -> Vec<PriceFeed> {
        let mut params = HashMap::new();
        params.insert("ids", price_ids.join(","));
        params.insert("publish_time", publish_time.second().to_string());

        let url = format!("{}/api/get_vaa", self.ws_endpoint);
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

    /// Fetch the PriceFeed of the given price id that is published since the given publish time.
    /// This will throw an error if the given publish time is in the future, or if the publish time
    /// is old and the price service endpoint does not have a db backend for historical requests.
    /// This will throw an axios error if there is a network problem or the price service returns a non-ok response (e.g: Invalid price id)
    pub async fn get_price_feed(
        &self,
        price_ids: &[&str],
        publish_time: DateTime<Utc>,
    ) -> Vec<PriceFeed> {
        let mut params = HashMap::new();
        params.insert("ids", price_ids.join(","));
        params.insert("publish_time", publish_time.second().to_string());

        let url = format!("{}/api/get_price_feed", self.ws_endpoint);
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
