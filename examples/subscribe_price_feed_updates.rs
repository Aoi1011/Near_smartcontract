use std::collections::HashMap;

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

    // let mut counter = HashMap::new();
    // let mut total_counter = 0;

    let price_ids: Vec<&str> = ids[0..2].iter().map(|price_id| price_id.as_str()).collect();
    connection
        .subscribe_price_feed_updates(&price_ids, |price_feed| {
            assert!(price_feed.get_metadata().is_some());
            assert!(price_feed.get_vaa().is_some());

            log::info!("Price feed: {price_feed:?}");

            // counter
            //     .entry(price_feed.id.to_string())
            //     .and_modify(|counter| *counter += 1)
            //     .or_insert(1);
            // total_counter += 1;
        })
        .await;

    // tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
    // connection.close_web_socket().await;

    // assert_eq!(total_counter, 30);

    // for id in ids {
    //     assert!(counter.get(&id).is_some());
    // }
}
