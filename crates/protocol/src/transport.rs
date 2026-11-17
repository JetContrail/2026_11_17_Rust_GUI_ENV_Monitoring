//! Message links over any async byte stream.
//!
//! Records are length-prefixed (`u32`, big endian, at most [`MAX_RECORD`] bytes) and carry
//! JSON messages. A link is either over a TLS stream ([`Link::tls`]) or protects each record
//! with AES-256-GCM ([`Link::aes_host`] / [`Link::aes_device`]); there is no plaintext mode.
//!
//! The AES handshake exchanges `HELLO_MAGIC | salt (16)` in both directions (device first)
//! and derives the session key from the pre-shared key and both salts.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_rustls::TlsStream;

use crate::crypto::{CryptoError, PresharedKey, Role, SALT_LEN, SecureChannel, random_salt};

pub const MAX_RECORD: usize = 64 * 1024;
pub const HELLO_MAGIC: [u8; 4] = *b"EMH1";

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("record of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("invalid message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("peer did not start an encrypted session")]
    NoHandshake,
}

pub async fn write_record<S: AsyncWrite + Unpin>(
    s: &mut S,
    payload: &[u8],
) -> Result<(), LinkError> {
    if payload.len() > MAX_RECORD {
        return Err(LinkError::TooLarge(payload.len()));
    }
    let len = u32::try_from(payload.len()).map_err(|_| LinkError::TooLarge(payload.len()))?;
    s.write_all(&len.to_be_bytes()).await?;
    s.write_all(payload).await?;
    s.flush().await?;
    Ok(())
}

pub async fn read_record<S: AsyncRead + Unpin>(s: &mut S) -> Result<Vec<u8>, LinkError> {
    let mut len = [0; 4];
    s.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_RECORD {
        return Err(LinkError::TooLarge(len));
    }
    let mut buf = vec![0; len];
    s.read_exact(&mut buf).await?;
    Ok(buf)
}

#[derive(Debug)]
pub struct Link<S> {
    stream: S,
    cipher: Option<SecureChannel>,
}

impl<T: AsyncRead + AsyncWrite + Unpin> Link<TlsStream<T>> {
    pub fn tls(stream: impl Into<TlsStream<T>>) -> Self {
        Self {
            stream: stream.into(),
            cipher: None,
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Link<S> {
    /// Host side of the AES-256-GCM handshake.
    pub async fn aes_host(mut stream: S, psk: &PresharedKey) -> Result<Self, LinkError> {
        let device_salt = parse_hello(&read_record(&mut stream).await?)?;
        let host_salt = random_salt()?;
        write_record(&mut stream, &hello(&host_salt)).await?;
        let cipher = SecureChannel::new(psk, Role::Host, &host_salt, &device_salt)?;
        Ok(Self {
            stream,
            cipher: Some(cipher),
        })
    }

    /// Device side of the AES-256-GCM handshake.
    pub async fn aes_device(mut stream: S, psk: &PresharedKey) -> Result<Self, LinkError> {
        let device_salt = random_salt()?;
        write_record(&mut stream, &hello(&device_salt)).await?;
        let host_salt = parse_hello(&read_record(&mut stream).await?)?;
        let cipher = SecureChannel::new(psk, Role::Device, &host_salt, &device_salt)?;
        Ok(Self {
            stream,
            cipher: Some(cipher),
        })
    }

    pub async fn send<M: Serialize>(&mut self, message: &M) -> Result<(), LinkError> {
        let json = serde_json::to_vec(message)?;
        let record = match &mut self.cipher {
            Some(c) => c.seal(&json)?,
            None => json,
        };
        write_record(&mut self.stream, &record).await
    }

    pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<M, LinkError> {
        let record = read_record(&mut self.stream).await?;
        let json = match &mut self.cipher {
            Some(c) => c.open(&record)?,
            None => record,
        };
        Ok(serde_json::from_slice(&json)?)
    }

    pub fn get_ref(&self) -> &S {
        &self.stream
    }
}

fn hello(salt: &[u8; SALT_LEN]) -> Vec<u8> {
    let mut out = HELLO_MAGIC.to_vec();
    out.extend_from_slice(salt);
    out
}

fn parse_hello(record: &[u8]) -> Result<[u8; SALT_LEN], LinkError> {
    match record.split_first_chunk::<4>() {
        Some((magic, salt)) if *magic == HELLO_MAGIC => {
            salt.try_into().map_err(|_| LinkError::NoHandshake)
        }
        _ => Err(LinkError::NoHandshake),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;
    use crate::message::Uplink;
    use envmon_core::model::DoorId;

    const MSG: Uplink = Uplink::DoorSensor {
        door: DoorId(1),
        open: true,
    };

    #[tokio::test]
    async fn aes_link_exchanges_messages() {
        let key = PresharedKey::generate().unwrap();
        let (a, b) = duplex(4096);
        let k2 = key.clone();
        let host = tokio::spawn(async move {
            let mut link = Link::aes_host(a, &k2).await.unwrap();
            let m: Uplink = link.recv().await.unwrap();
            link.send(&m).await.unwrap();
        });
        let mut device = Link::aes_device(b, &key).await.unwrap();
        device.send(&MSG).await.unwrap();
        assert_eq!(device.recv::<Uplink>().await.unwrap(), MSG);
        host.await.unwrap();
    }

    #[tokio::test]
    async fn aes_link_with_wrong_key_fails_authentication() {
        let (a, b) = duplex(4096);
        let host = tokio::spawn(async move {
            let mut link = Link::aes_host(a, &PresharedKey::new([1; 32]))
                .await
                .unwrap();
            link.recv::<Uplink>().await
        });
        let mut device = Link::aes_device(b, &PresharedKey::new([2; 32]))
            .await
            .unwrap();
        device.send(&MSG).await.unwrap();
        assert!(matches!(
            host.await.unwrap(),
            Err(LinkError::Crypto(CryptoError::Authentication))
        ));
    }

    #[tokio::test]
    async fn plaintext_peer_is_rejected() {
        let key = PresharedKey::generate().unwrap();
        // Peer skips the handshake and sends a plaintext JSON record.
        let (a, mut b) = duplex(4096);
        let host = tokio::spawn(async move { Link::aes_host(a, &key).await });
        write_record(&mut b, &serde_json::to_vec(&MSG).unwrap())
            .await
            .unwrap();
        assert!(matches!(host.await.unwrap(), Err(LinkError::NoHandshake)));

        // Peer completes the handshake but then sends plaintext.
        let key = PresharedKey::generate().unwrap();
        let (a, mut b) = duplex(4096);
        let k2 = key.clone();
        let host = tokio::spawn(async move {
            let mut link = Link::aes_host(a, &k2).await.unwrap();
            link.recv::<Uplink>().await
        });
        write_record(&mut b, &hello(&[0; SALT_LEN])).await.unwrap();
        read_record(&mut b).await.unwrap();
        write_record(&mut b, &serde_json::to_vec(&MSG).unwrap())
            .await
            .unwrap();
        assert!(matches!(
            host.await.unwrap(),
            Err(LinkError::Crypto(CryptoError::Plaintext))
        ));

        // Raw unframed JSON reads as an oversized length prefix.
        let (mut a, mut b) = duplex(4096);
        b.write_all(br#"{"type":"heartbeat"}"#).await.unwrap();
        assert!(matches!(
            read_record(&mut a).await,
            Err(LinkError::TooLarge(_))
        ));
    }
}
