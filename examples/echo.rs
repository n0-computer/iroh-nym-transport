//! Example echo server/client using iroh over Nym mixnet.
//!
//! This demonstrates bidirectional communication through the Nym mixnet.
//! Since Nym addresses are 96 bytes and not derivable from the endpoint key,
//! we use a simple "ticket" format to exchange connection info.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use iroh::{
    Endpoint, SecretKey,
    endpoint::{AckFrequencyConfig, Connection, QuicTransportConfig, VarInt},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_base::{EndpointAddr, TransportAddr};
use iroh_nym_transport::{NymAddr, NymUserTransport};
use iroh_tickets::endpoint::EndpointTicket;
use nym_sdk::mixnet::MixnetClient;
const ALPN: &[u8] = b"iroh-nym/echo/0";
const CHUNK_SIZE: usize = 64 * 1024; // 64 KiB chunks

/// QUIC tuning for Nym network: reduce ACK overhead.
/// Default ACKs every 2 packets = 50% overhead. This changes to every 11 packets = ~9% overhead.
fn nym_transport_config() -> QuicTransportConfig {
    let mut ack_config = AckFrequencyConfig::default();
    ack_config
        .ack_eliciting_threshold(VarInt::from_u32(10)) // ACK every 11 packets instead of 2
        .reordering_threshold(VarInt::from_u32(10)); // Tolerate more out-of-order packets

    QuicTransportConfig::builder()
        .ack_frequency_config(Some(ack_config))
        .build()
}

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        tracing::info!("Accepted connection from {}", connection.remote_id());
        let (mut send, mut recv) = connection.accept_bi().await?;

        let start = Instant::now();
        let mut total = 0u64;
        let mut buf = vec![0u8; CHUNK_SIZE];

        // Stream data back as we receive it
        loop {
            let n = match recv
                .read(&mut buf)
                .await
                .map_err(|e| AcceptError::from(std::io::Error::other(e.to_string())))?
            {
                Some(n) => n,
                None => break,
            };
            if n == 0 {
                break;
            }
            send.write_all(&buf[..n])
                .await
                .map_err(|e| AcceptError::from(std::io::Error::other(e.to_string())))?;
            total += n as u64;

            if total % (256 * 1024) == 0 {
                tracing::info!("  echoed {} KiB...", total / 1024);
            }
        }

        send.finish()?;
        let elapsed = start.elapsed();
        let throughput = if elapsed.as_secs_f64() > 0.0 {
            (total as f64 / 1024.0) / elapsed.as_secs_f64()
        } else {
            0.0
        };
        tracing::info!(
            "Echoed {} KiB in {:.2}s ({:.1} KiB/s)",
            total / 1024,
            elapsed.as_secs_f64(),
            throughput
        );

        connection.closed().await;
        Ok(())
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("accept") => run_accept().await,
        Some("connect") => {
            let ticket = args
                .get(2)
                .context("usage: echo connect <ticket> [size_kib]")?;
            let size_kib: usize = args
                .get(3)
                .map(|s| s.parse().unwrap_or(1024))
                .unwrap_or(1024); // Default 1 MiB
            run_connect(ticket, size_kib).await
        }
        _ => {
            println!("Nym Echo Example");
            println!();
            println!("Usage:");
            println!("  1. Start acceptor:  cargo run --example echo accept");
            println!("  2. Copy the ticket");
            println!("  3. Start connector: cargo run --example echo connect <ticket> [size_kib]");
            println!();
            println!("Options:");
            println!("  size_kib  Amount of data to send in KiB (default: 1024 = 1 MiB)");
            Ok(())
        }
    }
}

async fn run_accept() -> Result<()> {
    tracing::info!("Connecting to Nym mixnet...");
    let nym_client = MixnetClient::connect_new().await?;
    let nym_addr = NymAddr::from_recipient(nym_client.nym_address());
    tracing::info!("Nym address: {}", nym_addr);

    let transport = Arc::new(NymUserTransport::new(nym_client));
    let secret = SecretKey::generate(&mut rand::rng());
    let endpoint_id = secret.public();

    let ep = Endpoint::builder()
        .secret_key(secret)
        .transport_config(nym_transport_config())
        .clear_ip_transports()
        .clear_relay_transports()
        .clear_address_lookup()
        .add_custom_transport(transport.clone())
        .bind()
        .await?;

    // Create ticket with our endpoint info
    let custom_addr = nym_addr.to_custom_addr();
    let addr = EndpointAddr::from_parts(endpoint_id, [TransportAddr::Custom(custom_addr)]);
    let ticket = EndpointTicket::new(addr);

    tracing::info!("");
    tracing::info!("========================================");
    tracing::info!("Ticket (copy this for connector):");
    tracing::info!("{ticket}");
    tracing::info!("========================================");
    tracing::info!("");

    let _router = Router::builder(ep).accept(ALPN, Echo).spawn();
    tracing::info!("Accepting connections (Ctrl-C to exit)...");
    tokio::signal::ctrl_c().await?;

    Ok(())
}

async fn run_connect(ticket_str: &str, size_kib: usize) -> Result<()> {
    let ticket: EndpointTicket = ticket_str.parse().context("invalid ticket")?;
    let addr = ticket.endpoint_addr();
    tracing::info!("Connecting to endpoint: {}", addr.id);

    tracing::info!("Connecting to Nym mixnet...");
    let nym_client = MixnetClient::connect_new().await?;
    let transport = Arc::new(NymUserTransport::new(nym_client));

    let secret = SecretKey::generate(&mut rand::rng());
    let ep = Endpoint::builder()
        .secret_key(secret)
        .transport_config(nym_transport_config())
        .clear_ip_transports()
        .clear_relay_transports()
        .clear_address_lookup()
        .add_custom_transport(transport)
        .bind()
        .await?;

    tracing::info!("Connecting...");
    let conn = ep.connect(addr.clone(), ALPN).await?;
    tracing::info!("Connected!");

    let (mut send, mut recv) = conn.open_bi().await?;

    // Generate test data
    let total_size = size_kib * 1024;
    tracing::info!("Sending {} KiB of data...", size_kib);

    let start = Instant::now();

    // Send data in chunks
    let mut sent = 0usize;
    let mut chunk = vec![0u8; CHUNK_SIZE.min(total_size)];
    // Fill with pattern for verification
    for (i, b) in chunk.iter_mut().enumerate() {
        *b = (i % 256) as u8;
    }

    while sent < total_size {
        let to_send = CHUNK_SIZE.min(total_size - sent);
        send.write_all(&chunk[..to_send]).await?;
        sent += to_send;

        if sent % (256 * 1024) == 0 || sent == total_size {
            tracing::info!("  buffered {} KiB...", sent / 1024);
        }
    }
    send.finish()?;
    tracing::info!("Data buffered, waiting for echo...");

    // Receive echoed data
    tracing::info!("Receiving echo...");
    let recv_start = Instant::now();
    let mut last_recv = recv_start;
    let mut received = 0usize;
    let mut recv_buf = vec![0u8; CHUNK_SIZE];
    let mut errors = 0usize;
    let mut recv_count = 0usize;

    loop {
        let n = match recv.read(&mut recv_buf).await? {
            Some(n) => n,
            None => break,
        };
        if n == 0 {
            break;
        }
        recv_count += 1;
        let now = Instant::now();
        let since_last = now.duration_since(last_recv);
        let since_start = now.duration_since(recv_start);

        tracing::info!(
            "[recv #{}] {} bytes after {:.0}ms (total: {} KiB in {:.1}s)",
            recv_count,
            n,
            since_last.as_secs_f64() * 1000.0,
            (received + n) / 1024,
            since_start.as_secs_f64()
        );
        last_recv = now;

        // Verify data
        for (i, &b) in recv_buf[..n].iter().enumerate() {
            let expected = ((received + i) % 256) as u8;
            if b != expected {
                errors += 1;
            }
        }

        received += n;
    }

    let _recv_elapsed = recv_start.elapsed();
    let total_elapsed = start.elapsed();

    tracing::info!("");
    tracing::info!("========================================");
    tracing::info!("Results:");
    tracing::info!("  Data size:      {} KiB", size_kib);
    tracing::info!("  Received:       {} KiB", received / 1024);
    tracing::info!("  Total time:     {:.2}s", total_elapsed.as_secs_f64());

    let throughput = if total_elapsed.as_secs_f64() > 0.0 {
        (size_kib as f64 * 2.0) / total_elapsed.as_secs_f64() // *2 for send+receive
    } else {
        0.0
    };
    tracing::info!("  Throughput:     {:.1} KiB/s (round-trip)", throughput);

    // One-way latency estimate (total time / 2 trips / number of packets)
    let packets_approx = (size_kib * 1024) / 1200; // ~1200 bytes per QUIC packet
    if packets_approx > 0 {
        let latency_per_packet = total_elapsed.as_secs_f64() / 2.0 / packets_approx as f64;
        tracing::info!(
            "  Latency/packet: ~{:.0}ms (estimated)",
            latency_per_packet * 1000.0
        );
    }

    if errors > 0 {
        tracing::info!("  ERRORS:         {} bytes corrupted!", errors);
    } else {
        tracing::info!("  Verification:   OK");
    }
    tracing::info!("========================================");

    conn.close(0u8.into(), b"done");
    Ok(())
}
