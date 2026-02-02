//! Simple Nym mixnet hello world example.
//!
//! This creates a Nym client, sends a message to itself through the mixnet,
//! and receives it back.
//!
//! Run with: cargo run --example hello

use nym_sdk::mixnet::{MixnetClient, MixnetMessageSender};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    println!("Connecting to Nym mixnet...");

    // Create and connect a new ephemeral client
    // This will auto-select a gateway and generate temporary keys
    let mut client = MixnetClient::connect_new().await?;

    // Get our address - this is what others would use to send to us
    let our_address = *client.nym_address();
    println!("Our Nym address: {our_address}");

    // Send a message to ourselves through the mixnet
    let message = "Hello from the Nym mixnet!";
    println!("Sending: {message}");

    client.send_plain_message(our_address, message).await?;

    println!("Waiting for message...");

    // Wait for the message to come back through the mixnet
    // This can take a few seconds due to mixing delays
    if let Some(messages) = client.wait_for_messages().await {
        for msg in messages {
            let text = String::from_utf8_lossy(&msg.message);
            println!("Received: {text}");
        }
    }

    // Clean disconnect
    client.disconnect().await;
    println!("Done!");

    Ok(())
}
