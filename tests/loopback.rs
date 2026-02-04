//! Loopback integration test - runs both accept and connect in one process.

use std::sync::Arc;

use iroh::{
    Endpoint, SecretKey,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_base::{EndpointAddr, TransportAddr};
use iroh_nym_transport::{NymAddr, NymUserTransport};
use nym_sdk::mixnet::MixnetClient;

const ALPN: &[u8] = b"iroh-nym/echo/0";

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        eprintln!("[echo] accepted connection from {}", connection.remote_id());
        let (mut send, mut recv) = connection.accept_bi().await?;
        let data = recv
            .read_to_end(4096)
            .await
            .map_err(|e| AcceptError::from(std::io::Error::other(e.to_string())))?;
        eprintln!("[echo] received: {}", String::from_utf8_lossy(&data));
        send.write_all(&data)
            .await
            .map_err(|e| AcceptError::from(std::io::Error::other(e.to_string())))?;
        send.finish()?;
        connection.closed().await;
        eprintln!("[echo] connection closed");
        Ok(())
    }
}

#[tokio::test]
async fn loopback_echo() {
    eprintln!("[test] Starting loopback test...");

    // Create two Nym clients
    eprintln!("[test] Connecting first Nym client (acceptor)...");
    let nym_client1 = MixnetClient::connect_new()
        .await
        .expect("failed to connect acceptor");
    let nym_addr1 = NymAddr::from_recipient(nym_client1.nym_address());
    eprintln!("[test] Acceptor Nym address: {}", nym_addr1);

    eprintln!("[test] Connecting second Nym client (connector)...");
    let nym_client2 = MixnetClient::connect_new()
        .await
        .expect("failed to connect connector");
    let nym_addr2 = NymAddr::from_recipient(nym_client2.nym_address());
    eprintln!("[test] Connector Nym address: {}", nym_addr2);

    // Create acceptor endpoint
    let transport1 = Arc::new(NymUserTransport::new(nym_client1));
    let secret1 = SecretKey::generate(&mut rand::rng());
    let endpoint_id1 = secret1.public();

    let ep1 = Endpoint::builder()
        .secret_key(secret1)
        .clear_ip_transports()
        .clear_relay_transports()
        .clear_address_lookup()
        .add_custom_transport(transport1)
        .bind()
        .await
        .expect("failed to bind acceptor");

    // Start router for acceptor
    let _router = Router::builder(ep1.clone()).accept(ALPN, Echo).spawn();
    eprintln!("[test] Acceptor endpoint ready: {}", endpoint_id1);

    // Create connector endpoint
    let transport2 = Arc::new(NymUserTransport::new(nym_client2));
    let secret2 = SecretKey::generate(&mut rand::rng());

    let ep2 = Endpoint::builder()
        .secret_key(secret2)
        .clear_ip_transports()
        .clear_relay_transports()
        .clear_address_lookup()
        .add_custom_transport(transport2)
        .bind()
        .await
        .expect("failed to bind connector");
    eprintln!("[test] Connector endpoint ready");

    // Build the target address
    let custom_addr = nym_addr1.to_custom_addr();
    let target_addr = EndpointAddr::from_parts(endpoint_id1, [TransportAddr::Custom(custom_addr)]);

    // Give the Nym network a moment to stabilize
    eprintln!("[test] Waiting 2 seconds for Nym network to stabilize...");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Connect
    eprintln!("[test] Connecting to acceptor...");
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        ep2.connect(target_addr, ALPN),
    )
    .await
    .expect("connect timed out")
    .expect("connect failed");
    eprintln!("[test] Connected!");

    let (mut send, mut recv) = conn.open_bi().await.expect("open_bi failed");
    let message = b"Hello from loopback test!";
    eprintln!("[test] Sending: {}", String::from_utf8_lossy(message));
    send.write_all(message).await.expect("write failed");
    send.finish().expect("finish failed");

    let response = recv.read_to_end(4096).await.expect("read failed");
    eprintln!(
        "[test] Echo response: {}",
        String::from_utf8_lossy(&response)
    );

    assert_eq!(response, message);

    conn.close(0u8.into(), b"done");
    eprintln!("[test] Test completed successfully!");
}
