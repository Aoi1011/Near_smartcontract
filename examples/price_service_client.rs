use price_service_client::price_service_connection::{
    PriceServiceConnection, PriceServiceConnectionConfig,
};

#[tokio::main]
async fn main() {
    let config = PriceServiceConnectionConfig::default();
    let connection = PriceServiceConnection::new("https://hermes.pyth.network", Some(config));

    let price_feeds = connection
        .get_latest_price_feeds(&[
            "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
        ])
        .await
        .expect("get latest price feeds");
    println!("Price Feeds: {price_feeds:?}");

    let vaas = connection
        .get_latest_vass(&["e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43"])
        .await
        .expect("get latest vaas");
    println!("VAAs: {vaas:?}");
}
