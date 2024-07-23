use price_service_client::price_service_connection::PriceServiceConnection;

#[tokio::main]
async fn main() {
    env_logger::init();

    let mut connection = PriceServiceConnection::new("https://hermes.pyth.network", None)
        .expect("Failed to construct");

    let ids = connection
        .get_price_feed_ids()
        .await
        .expect("Failed to get price feed ids");
    assert!(!ids.is_empty());

    let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();
    log::debug!("Price ids: {price_ids:?}");

    connection
        .subscribe_price_feed_updates(&price_ids, |price_feed| {
            let id: String = hex::encode(price_feed.id.0);
            log::info!("Current Price for {}: {}", id, price_feed.price.price);
        })
        .await
        .expect("Failed to subscribe");

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
    }

    // connection.unsu
}
