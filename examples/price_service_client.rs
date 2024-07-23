use chrono::DateTime;
use price_service_client::{
    price_service_connection::{PriceServiceConnection, PriceServiceConnectionConfig},
    types::RpcPriceFeed,
};

type Cb = fn(RpcPriceFeed);

#[tokio::main]
async fn main() {
    let config = PriceServiceConnectionConfig::default();
    let connection: PriceServiceConnection<Cb> =
        PriceServiceConnection::new("https://hermes.pyth.network", Some(config))
            .expect("Failed to construct");

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

    let vaa = connection
        .get_vaa(
            "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
            DateTime::from_timestamp_nanos(1690576641),
        )
        .await;
    println!("VAAs: {vaa:?}");

    let price_feed = connection
        .get_price_feed(
            "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
            DateTime::from_timestamp_nanos(1717632000),
        )
        .await
        .expect("get price feed");
    println!("Price feed: {price_feed:?}");
}
