# iroh-nym-transport

A custom transport for [iroh](https://github.com/n0-computer/iroh) that routes QUIC packets through the [Nym](https://nymtech.net/) mixnet for enhanced privacy.

> **Experimental:** both iroh custom transports and this crate are experimental and may change.

## What is this?

This crate implements iroh's `CustomTransport` trait to tunnel iroh connections through the Nym mixnet. Instead of packets traveling directly between peers over the internet, they are routed through a network of mix nodes that shuffle and delay packets, making traffic analysis extremely difficult.

## Tradeoffs

### Privacy vs Performance

| Aspect | Direct Connection | Nym Transport |
|--------|------------------|---------------|
| **Latency** | ~50-200ms typical | ~1-3 seconds RTT |
| **Throughput** | 10+ Mbps | ~15-20 KiB/s (testnet) |
| **Privacy** | IP visible to peer | IP hidden, traffic analysis resistant |
| **Metadata** | Timing correlations possible | Timing obfuscated by mixing |

### Why so slow?

1. **Mixing delays**: Nym intentionally delays packets at each hop to prevent timing correlation attacks. This is a feature, not a bug.

2. **Multiple hops**: Packets traverse 3 mix nodes (entry, middle, exit) plus gateways on each end.

3. **Rate limiting**: The Nym testnet limits packet rate to ~50 packets/second per client.

4. **No packet coalescing**: Unlike UDP over the internet, each QUIC packet becomes a separate Nym packet with its own mixing delays.

### When to use this

**Good for:**
- Privacy-sensitive communications where latency is acceptable
- Situations where IP address exposure is a concern
- Censorship-resistant peer discovery
- Low-bandwidth control channels, chat, or signaling

**Not suitable for:**
- Bulk data transfer
- Real-time applications (voice, video, gaming)
- High-throughput file sync
- Anything requiring sub-second latency

### QUIC over Nym considerations

QUIC was designed for low-latency internet connections. Running it over Nym requires tuning:

- **ACK frequency**: Default QUIC ACKs every 2 packets (50% overhead). We reduce this to every 11 packets (~9% overhead) since ACKs are expensive over Nym.

- **Congestion control**: QUIC's congestion controller assumes ~300ms RTT. With Nym's 1-3s RTT, it takes longer to ramp up. The backpressure in this transport prevents packet drops.

- **Flow control windows**: Larger windows help with high-latency links, but too large causes the send queue to back up waiting for Nym's rate limit.

## Usage

```rust
use std::sync::Arc;
use iroh::{Endpoint, SecretKey};
use iroh_nym_transport::{NymAddr, NymUserTransport};
use nym_sdk::mixnet::MixnetClient;

// Connect to Nym mixnet
let nym_client = MixnetClient::connect_new().await?;
let nym_addr = NymAddr::from_recipient(nym_client.nym_address());

// Create transport and endpoint
let transport = Arc::new(NymUserTransport::new(nym_client));
let secret = SecretKey::generate(&mut rand::rng());

let endpoint = Endpoint::builder()
    .secret_key(secret)
    .clear_ip_transports()      // Disable direct UDP
    .clear_relay_transports()   // Disable relays
    .add_custom_transport(transport)
    .bind()
    .await?;

// Now use the endpoint normally - traffic goes through Nym
```

## Examples

```bash
# Start an echo server
cargo run --example echo accept

# Connect to it (copy the ticket from the server output)
cargo run --example echo connect <ticket> [size_kib]
```

## Requirements

- Rust 2024 edition
- Active Nym network connection (testnet or mainnet)
- The `iroh` crate with custom transport support (currently on `feat-user-transport-2` branch)

## License

Copyright 2025 N0, INC.

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
