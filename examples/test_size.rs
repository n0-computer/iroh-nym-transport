//! Test message size limits in Nym SDK.
//!
//! This sends messages of various sizes to ourselves to see what the actual limit is.

use nym_sdk::mixnet::{MixnetClient, MixnetMessageSender};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    println!("Connecting to Nym mixnet...");
    let mut client = MixnetClient::connect_new().await?;
    let our_address = *client.nym_address();
    println!("Connected! Address: {our_address}");

    // Test various sizes
    let sizes = [100, 500, 900, 1000, 1007, 1024, 1500, 2000, 2048, 4096];

    for size in sizes {
        let msg = vec![0xABu8; size];
        println!("\nSending {size} byte message...");

        match client.send_plain_message(our_address, msg).await {
            Ok(()) => println!("  Sent OK"),
            Err(e) => println!("  Send failed: {e}"),
        }
    }

    println!("\nWaiting for messages (10 seconds)...");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.wait_for_messages(),
        )
        .await
        {
            Ok(Some(messages)) => {
                for msg in messages {
                    println!("  Received {} bytes", msg.message.len());
                }
            }
            Ok(None) => break,
            Err(_) => {} // timeout, continue
        }
    }

    client.disconnect().await;
    println!("\nDone!");

    Ok(())
}
