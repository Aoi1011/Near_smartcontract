use reqwest::Client;

pub mod resilient_web_socket;

pub struct PriceServiceConnection {
    http_client: Client,
}

