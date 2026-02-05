//! Nym mixnet transport for iroh.
//!
//! This crate provides a custom transport for iroh that routes packets through
//! the Nym mixnet.

use std::{future::Future, io, num::NonZeroUsize, pin::Pin, str::FromStr, sync::Arc};

use bytes::Bytes;
use iroh::endpoint::{
    Builder,
    presets::Preset,
    transports::{Addr, CustomEndpoint, CustomSender, CustomTransport, Transmit},
};
use iroh_base::CustomAddr;
use n0_watcher::Watchable;
use nym_sdk::mixnet::{MixnetClient, MixnetClientSender, MixnetMessageSender, Recipient};
use quinn_udp::RecvMeta;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Transport ID for the Nym user transport.
/// 0x4E594D = "NYM" in ASCII.
pub const NYM_TRANSPORT_ID: u64 = 0x4E594D;

/// Size of a Nym address in bytes.
pub const NYM_ADDR_SIZE: usize = 96;

/// Convert a Nym Recipient to an iroh CustomAddr.
pub fn to_custom_addr(recipient: &Recipient) -> CustomAddr {
    CustomAddr::from_parts(NYM_TRANSPORT_ID, &recipient.to_bytes())
}

/// Convert an iroh CustomAddr back to a Nym Recipient.
pub fn from_custom_addr(addr: &CustomAddr) -> Option<Recipient> {
    if addr.id() != NYM_TRANSPORT_ID {
        return None;
    }
    if addr.data().len() != NYM_ADDR_SIZE {
        return None;
    }
    let bytes: [u8; NYM_ADDR_SIZE] = addr.data().try_into().ok()?;
    Recipient::try_from_bytes(bytes).ok()
}

/// Check if a CustomAddr is a valid Nym address.
pub fn is_nym_addr(addr: &CustomAddr) -> bool {
    addr.id() == NYM_TRANSPORT_ID && addr.data().len() == NYM_ADDR_SIZE
}

/// A Nym address wrapper for use with iroh custom transports.
///
/// This is a 96-byte address containing:
/// - Client identity key (32 bytes, Ed25519)
/// - Client encryption key (32 bytes, X25519)
/// - Gateway identity key (32 bytes, Ed25519)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NymAddr(pub [u8; NYM_ADDR_SIZE]);

impl NymAddr {
    /// Create from raw bytes.
    pub fn from_bytes(bytes: [u8; NYM_ADDR_SIZE]) -> Self {
        Self(bytes)
    }

    /// Get the raw bytes.
    pub fn as_bytes(&self) -> &[u8; NYM_ADDR_SIZE] {
        &self.0
    }

    /// Convert to a Nym SDK Recipient.
    pub fn to_recipient(&self) -> Option<Recipient> {
        Recipient::try_from_bytes(self.0).ok()
    }

    /// Create from a Nym SDK Recipient.
    pub fn from_recipient(recipient: &Recipient) -> Self {
        Self(recipient.to_bytes())
    }

    /// Convert to an iroh CustomAddr.
    pub fn to_custom_addr(&self) -> CustomAddr {
        CustomAddr::from_parts(NYM_TRANSPORT_ID, &self.0)
    }

    /// Create from an iroh CustomAddr.
    pub fn from_custom_addr(addr: &CustomAddr) -> Option<Self> {
        if addr.id() != NYM_TRANSPORT_ID || addr.data().len() != NYM_ADDR_SIZE {
            return None;
        }
        let bytes: [u8; NYM_ADDR_SIZE] = addr.data().try_into().ok()?;
        Some(Self(bytes))
    }
}

impl std::fmt::Display for NymAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(recipient) = self.to_recipient() {
            write!(f, "{recipient}")
        } else {
            for b in &self.0 {
                write!(f, "{b:02x}")?;
            }
            Ok(())
        }
    }
}

impl FromStr for NymAddr {
    type Err = AddrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let recipient = Recipient::from_str(s).map_err(|_| AddrParseError::InvalidFormat)?;
        Ok(Self::from_recipient(&recipient))
    }
}

/// Error parsing a Nym address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrParseError {
    /// Invalid address format.
    InvalidFormat,
    /// Wrong transport ID (not a Nym address).
    WrongTransportId,
}

impl std::fmt::Display for AddrParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFormat => write!(f, "invalid Nym address format"),
            Self::WrongTransportId => write!(f, "wrong transport ID"),
        }
    }
}

impl std::error::Error for AddrParseError {}

/// A packet carried over the Nym mixnet transport.
///
/// Contains the source address, optional segment size for GSO batching,
/// and the payload data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NymPacket {
    /// Source Nym address (96 bytes as a Vec for serde compatibility).
    #[serde(with = "serde_bytes")]
    pub from: Vec<u8>,
    /// Segment size for GSO batching (optional).
    pub segment_size: Option<u16>,
    /// Raw packet payload.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

impl NymPacket {
    /// Create a new packet.
    pub fn new(from: [u8; NYM_ADDR_SIZE], segment_size: Option<u16>, data: Vec<u8>) -> Self {
        Self {
            from: from.to_vec(),
            segment_size,
            data,
        }
    }

    /// Get the source address if valid (96 bytes).
    pub fn source_addr(&self) -> Option<[u8; NYM_ADDR_SIZE]> {
        self.from.as_slice().try_into().ok()
    }

    /// Serialize the packet using postcard.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("serialization should not fail")
    }

    /// Deserialize a packet from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

/// A Nym mixnet-backed user transport for iroh.
///
/// This wraps a connected `MixnetClient` and provides the iroh transport interface.
pub struct NymUserTransport {
    /// Our Nym address.
    local_addr: NymAddr,
    /// The sender handle - can be used concurrently without blocking receives.
    sender: MixnetClientSender,
    /// The connected Nym client behind a mutex (only used for receiving).
    client: Arc<Mutex<MixnetClient>>,
}

impl NymUserTransport {
    /// Create a new transport from a connected MixnetClient.
    ///
    /// The client should already be connected to the mixnet.
    pub fn new(client: MixnetClient) -> Self {
        let local_addr = NymAddr::from_recipient(client.nym_address());
        let sender = client.split_sender();
        Self {
            local_addr,
            sender,
            client: Arc::new(Mutex::new(client)),
        }
    }

    /// Get our Nym address.
    pub fn local_addr(&self) -> &NymAddr {
        &self.local_addr
    }

    /// Returns a preset that configures an endpoint to use this Nym transport.
    pub fn preset(self: Arc<Self>) -> impl Preset {
        NymPreset { transport: self }
    }
}

impl std::fmt::Debug for NymUserTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NymUserTransport")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

/// Capacity of the outgoing packet queue.
const SEND_QUEUE_CAPACITY: usize = 128;

impl CustomTransport for NymUserTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        let (recv_tx, recv_rx) = tokio::sync::mpsc::channel(64 * 1024);
        let (send_tx, mut send_rx) =
            tokio::sync::mpsc::channel::<OutgoingPacket>(SEND_QUEUE_CAPACITY);
        let watchable = Watchable::new(vec![self.local_addr.to_custom_addr()]);

        // Spawn receive loop
        let client = self.client.clone();
        tokio::spawn(async move {
            loop {
                let messages = {
                    let mut guard = client.lock().await;
                    guard.wait_for_messages().await
                };

                if let Some(messages) = messages {
                    for msg in messages {
                        tracing::trace!("received {} bytes from mixnet", msg.message.len());
                        let packet = Bytes::from(msg.message);
                        if recv_tx.send(packet).await.is_err() {
                            return; // Channel closed, stop receiving
                        }
                    }
                }
            }
        });

        // Spawn sender task with backpressure
        let sender = self.sender.clone();
        tokio::spawn(async move {
            while let Some(packet) = send_rx.recv().await {
                if let Err(e) = sender
                    .send_plain_message(packet.recipient, packet.payload)
                    .await
                {
                    tracing::warn!("send failed: {e}");
                }
            }
        });

        Ok(Box::new(NymUserEndpoint {
            local_addr: self.local_addr,
            send_tx,
            watchable,
            receiver: recv_rx,
        }))
    }
}

struct NymPreset {
    transport: Arc<NymUserTransport>,
}

impl Preset for NymPreset {
    fn apply(self, builder: Builder) -> Builder {
        builder.add_custom_transport(self.transport)
    }
}

/// Message to send via the mixnet.
struct OutgoingPacket {
    recipient: Recipient,
    payload: Vec<u8>,
}

/// Active Nym user endpoint.
struct NymUserEndpoint {
    local_addr: NymAddr,
    send_tx: tokio::sync::mpsc::Sender<OutgoingPacket>,
    watchable: Watchable<Vec<CustomAddr>>,
    receiver: tokio::sync::mpsc::Receiver<Bytes>,
}

impl std::fmt::Debug for NymUserEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NymUserEndpoint")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

type SendFuture = Pin<
    Box<
        dyn Future<Output = Result<(), tokio::sync::mpsc::error::SendError<OutgoingPacket>>>
            + Send
            + Sync,
    >,
>;

struct NymUserSender {
    local_addr: NymAddr,
    send_tx: tokio::sync::mpsc::Sender<OutgoingPacket>,
    /// Pending send operation for backpressure
    pending: Mutex<Option<SendFuture>>,
}

impl std::fmt::Debug for NymUserSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NymUserSender")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

impl CustomSender for NymUserSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        is_nym_addr(addr)
    }

    fn poll_send(
        &self,
        cx: &mut std::task::Context,
        dst: &CustomAddr,
        transmit: &Transmit<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        // First, check if we have a pending send and poll it
        let mut pending = self.pending.try_lock().ok();
        if let Some(ref mut guard) = pending
            && let Some(ref mut fut) = **guard {
                match fut.as_mut().poll(cx) {
                    std::task::Poll::Ready(Ok(())) => {
                        **guard = None; // Clear completed future
                        return std::task::Poll::Ready(Ok(()));
                    }
                    std::task::Poll::Ready(Err(_)) => {
                        **guard = None;
                        return std::task::Poll::Ready(Err(io::Error::other(
                            "send channel closed",
                        )));
                    }
                    std::task::Poll::Pending => {
                        return std::task::Poll::Pending;
                    }
                }
            }
        drop(pending); // Release lock before potentially creating new future

        let recipient =
            from_custom_addr(dst).ok_or_else(|| io::Error::other("invalid Nym address"))?;

        // Build packet
        let segment_size = transmit
            .segment_size
            .and_then(|s| u16::try_from(s).ok());

        let packet = NymPacket::new(self.local_addr.0, segment_size, transmit.contents.to_vec());

        let payload = packet.to_bytes();
        tracing::trace!("queueing {} bytes to {}", payload.len(), recipient);

        // Try immediate send first
        let packet = OutgoingPacket { recipient, payload };
        match self.send_tx.try_send(packet) {
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(tokio::sync::mpsc::error::TrySendError::Full(packet)) => {
                // Queue full - create async send future for backpressure
                let sender = self.send_tx.clone();
                let fut: SendFuture = Box::pin(async move { sender.send(packet).await });

                // Store and poll the future
                if let Ok(mut guard) = self.pending.try_lock() {
                    *guard = Some(fut);
                    if let Some(ref mut f) = *guard {
                        match f.as_mut().poll(cx) {
                            std::task::Poll::Ready(Ok(())) => {
                                *guard = None;
                                std::task::Poll::Ready(Ok(()))
                            }
                            std::task::Poll::Ready(Err(_)) => {
                                *guard = None;
                                std::task::Poll::Ready(Err(io::Error::other("send channel closed")))
                            }
                            std::task::Poll::Pending => std::task::Poll::Pending,
                        }
                    } else {
                        std::task::Poll::Pending
                    }
                } else {
                    // Couldn't get lock, return WouldBlock (rare race)
                    std::task::Poll::Ready(Err(io::Error::from(io::ErrorKind::WouldBlock)))
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                std::task::Poll::Ready(Err(io::Error::other("send channel closed")))
            }
        }
    }
}

impl CustomEndpoint for NymUserEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.watchable.watch()
    }

    fn max_transmit_segments(&self) -> NonZeroUsize {
        // Nym mixnet doesn't support GSO batching at the network level,
        // so we transmit one segment at a time
        NonZeroUsize::MIN
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(NymUserSender {
            local_addr: self.local_addr,
            send_tx: self.send_tx.clone(),
            pending: Mutex::new(None),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [RecvMeta],
        source_addrs: &mut [Addr],
    ) -> std::task::Poll<io::Result<usize>> {
        let n = bufs.len().min(metas.len()).min(source_addrs.len());
        if n == 0 {
            return std::task::Poll::Ready(Ok(0));
        }

        let mut filled = 0usize;
        while filled < n {
            match self.receiver.poll_recv(cx) {
                std::task::Poll::Pending => {
                    if filled == 0 {
                        return std::task::Poll::Pending;
                    }
                    break;
                }
                std::task::Poll::Ready(None) => {
                    return std::task::Poll::Ready(Err(io::Error::other("packet channel closed")));
                }
                std::task::Poll::Ready(Some(data)) => {
                    tracing::trace!("poll_recv got {} bytes from channel", data.len());
                    // Deserialize packet
                    let Some(packet) = NymPacket::from_bytes(&data) else {
                        tracing::warn!("poll_recv: malformed packet, skipping");
                        continue; // Malformed packet
                    };

                    let Some(source_bytes) = packet.source_addr() else {
                        tracing::warn!("poll_recv: invalid source address, skipping");
                        continue; // Invalid source address
                    };
                    let source = NymAddr(source_bytes);

                    if bufs[filled].len() < packet.data.len() {
                        tracing::warn!(
                            "poll_recv: buffer too small ({} < {}), skipping",
                            bufs[filled].len(),
                            packet.data.len()
                        );
                        continue; // Buffer too small
                    }

                    tracing::trace!(
                        "poll_recv: delivering {} bytes from {}",
                        packet.data.len(),
                        source
                    );
                    bufs[filled][..packet.data.len()].copy_from_slice(&packet.data);
                    metas[filled].len = packet.data.len();
                    metas[filled].stride = packet
                        .segment_size
                        .map(|s| s as usize)
                        .unwrap_or(packet.data.len());
                    source_addrs[filled] = Addr::Custom(source.to_custom_addr());
                    filled += 1;
                }
            }
        }

        if filled > 0 {
            std::task::Poll::Ready(Ok(filled))
        } else {
            std::task::Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_addr_roundtrip() {
        let mut bytes = [0u8; 96];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i * 7) as u8;
        }
        let addr = NymAddr::from_bytes(bytes);

        let custom = addr.to_custom_addr();
        assert!(is_nym_addr(&custom));

        let unpacked = NymAddr::from_custom_addr(&custom).unwrap();
        assert_eq!(addr, unpacked);
    }
}
