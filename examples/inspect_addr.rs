//! Inspect the structure of a Nym address.

use nym_sdk::mixnet::MixnetClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    println!("Connecting to get a Nym address...");

    let client = MixnetClient::connect_new().await?;
    let addr = client.nym_address();

    println!();
    println!("String representation:");
    println!("  {addr}");
    println!();

    let bytes = addr.to_bytes();
    println!("Binary representation: {} bytes total", bytes.len());
    println!();
    println!("  Client identity (0..32):    {}", hex(&bytes[0..32]));
    println!("  Client encryption (32..64): {}", hex(&bytes[32..64]));
    println!("  Gateway identity (64..96):  {}", hex(&bytes[64..96]));

    client.disconnect().await;

    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
