//! TLS 1.3 (rustls, ring provider). Device links require mutual certificate authentication;
//! the HTTPS WebAPI uses server authentication plus API tokens. Older TLS versions are not
//! offered. [`DevCa`] issues certificates for tests, the simulator and development setups.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::version::TLS13;
use rustls::{ClientConfig, RootCertStore, ServerConfig};

pub use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
    #[error("client certificate verifier: {0}")]
    Verifier(#[from] rustls::server::VerifierBuilderError),
    #[error("certificate generation: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("PEM: {0}")]
    Pem(#[from] rustls::pki_types::pem::Error),
    #[error("invalid server name {0:?}")]
    ServerName(String),
    #[error("no certificate found")]
    NoCertificate,
}

/// Certificate chain and private key of an endpoint.
pub struct Identity {
    pub chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("certificates", &self.chain.len())
            .finish_non_exhaustive()
    }
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            chain: self.chain.clone(),
            key: self.key.clone_key(),
        }
    }
}

impl Identity {
    pub fn new(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Self {
        Self { chain, key }
    }

    /// Loads a PEM certificate chain and PEM private key.
    pub fn from_pem_files(cert: impl AsRef<Path>, key: impl AsRef<Path>) -> Result<Self, TlsError> {
        let chain = load_certificates(cert)?;
        let key = PrivateKeyDer::from_pem_file(key)?;
        Ok(Self { chain, key })
    }

    pub fn key(&self) -> PrivateKeyDer<'static> {
        self.key.clone_key()
    }
}

pub fn load_certificates(path: impl AsRef<Path>) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let certs = CertificateDer::pem_file_iter(path)?.collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err(TlsError::NoCertificate);
    }
    Ok(certs)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

fn root_store(cas: &[CertificateDer<'static>]) -> Result<Arc<RootCertStore>, TlsError> {
    let mut roots = RootCertStore::empty();
    for ca in cas {
        roots.add(ca.clone())?;
    }
    Ok(Arc::new(roots))
}

/// Device-link server: TLS 1.3 only, client certificate signed by `client_cas` required.
pub fn mutual_server_config(
    identity: &Identity,
    client_cas: &[CertificateDer<'static>],
) -> Result<Arc<ServerConfig>, TlsError> {
    let verifier =
        WebPkiClientVerifier::builder_with_provider(root_store(client_cas)?, provider()).build()?;
    let config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(identity.chain.clone(), identity.key())?;
    Ok(Arc::new(config))
}

/// HTTPS server: TLS 1.3 only, no client certificate (callers authenticate with API tokens).
pub fn https_server_config(identity: &Identity) -> Result<Arc<ServerConfig>, TlsError> {
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])?
        .with_no_client_auth()
        .with_single_cert(identity.chain.clone(), identity.key())?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// TLS 1.3 client trusting `server_cas`, presenting `identity` when given.
pub fn client_config(
    server_cas: &[CertificateDer<'static>],
    identity: Option<&Identity>,
) -> Result<Arc<ClientConfig>, TlsError> {
    let builder = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])?
        .with_root_certificates(root_store(server_cas)?);
    let config = match identity {
        Some(id) => builder.with_client_auth_cert(id.chain.clone(), id.key())?,
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

pub fn server_name(name: &str) -> Result<ServerName<'static>, TlsError> {
    ServerName::try_from(name.to_owned()).map_err(|_| TlsError::ServerName(name.to_owned()))
}

/// Development certificate authority.
pub struct DevCa {
    params: CertificateParams,
    key: KeyPair,
    cert: CertificateDer<'static>,
    pem: String,
}

impl fmt::Debug for DevCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DevCa").finish_non_exhaustive()
    }
}

impl DevCa {
    pub fn generate(common_name: &str) -> Result<Self, TlsError> {
        let mut params = CertificateParams::new(Vec::<String>::new())?;
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key = KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        Ok(Self {
            params,
            key,
            cert: cert.der().clone(),
            pem: cert.pem(),
        })
    }

    pub fn certificate(&self) -> CertificateDer<'static> {
        self.cert.clone()
    }

    pub fn certificate_pem(&self) -> &str {
        &self.pem
    }

    /// Server certificate for the given DNS names / IP addresses.
    pub fn issue_server(&self, names: &[&str]) -> Result<Identity, TlsError> {
        let common_name = names.first().copied().unwrap_or("server");
        self.issue(names, common_name, ExtendedKeyUsagePurpose::ServerAuth)
    }

    /// Client certificate for a device or operator station.
    pub fn issue_client(&self, common_name: &str) -> Result<Identity, TlsError> {
        self.issue(&[], common_name, ExtendedKeyUsagePurpose::ClientAuth)
    }

    fn issue(
        &self,
        names: &[&str],
        common_name: &str,
        usage: ExtendedKeyUsagePurpose,
    ) -> Result<Identity, TlsError> {
        let mut params =
            CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())?;
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.extended_key_usages = vec![usage];
        params.use_authority_key_identifier_extension = true;
        let key = KeyPair::generate()?;
        let issuer = Issuer::from_params(&self.params, &self.key);
        let cert = params.signed_by(&key, &issuer)?;
        Ok(Identity {
            chain: vec![cert.der().clone()],
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        })
    }
}

#[cfg(test)]
mod tests {
    use rustls::ProtocolVersion;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    struct Pki {
        ca: DevCa,
        server: Identity,
        device: Identity,
    }

    fn pki() -> Pki {
        let ca = DevCa::generate("envmon test CA").unwrap();
        let server = ca.issue_server(&["localhost", "127.0.0.1"]).unwrap();
        let device = ca.issue_client("reader-1").unwrap();
        Pki { ca, server, device }
    }

    /// Runs one server handshake and returns the server-side result and the client stream.
    async fn handshake(
        server: Arc<ServerConfig>,
        client: Arc<ClientConfig>,
    ) -> (
        Result<tokio_rustls::server::TlsStream<TcpStream>, std::io::Error>,
        Result<tokio_rustls::client::TlsStream<TcpStream>, std::io::Error>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            TlsAcceptor::from(server).accept(tcp).await
        });
        let tcp = TcpStream::connect(addr).await.unwrap();
        let client = TlsConnector::from(client)
            .connect(server_name("localhost").unwrap(), tcp)
            .await;
        (accept.await.unwrap(), client)
    }

    #[tokio::test]
    async fn mutual_tls13_handshake_and_data_exchange() {
        let p = pki();
        let server_cfg = mutual_server_config(&p.server, &[p.ca.certificate()]).unwrap();
        let client_cfg = client_config(&[p.ca.certificate()], Some(&p.device)).unwrap();
        let (server, client) = handshake(server_cfg, client_cfg).await;
        let (mut server, mut client) = (server.unwrap(), client.unwrap());

        assert_eq!(
            server.get_ref().1.protocol_version(),
            Some(ProtocolVersion::TLSv1_3)
        );
        assert!(
            server
                .get_ref()
                .1
                .peer_certificates()
                .is_some_and(|c| c[0] == p.device.chain[0])
        );

        client.write_all(b"card 0123").await.unwrap();
        client.flush().await.unwrap();
        let mut buf = [0; 9];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"card 0123");
    }

    #[tokio::test]
    async fn client_without_certificate_is_rejected() {
        let p = pki();
        let server_cfg = mutual_server_config(&p.server, &[p.ca.certificate()]).unwrap();
        let client_cfg = client_config(&[p.ca.certificate()], None).unwrap();
        let (server, _) = handshake(server_cfg, client_cfg).await;
        assert!(server.is_err());
    }

    #[tokio::test]
    async fn client_certificate_from_another_ca_is_rejected() {
        let p = pki();
        let rogue = DevCa::generate("rogue CA")
            .unwrap()
            .issue_client("reader-1")
            .unwrap();
        let server_cfg = mutual_server_config(&p.server, &[p.ca.certificate()]).unwrap();
        let client_cfg = client_config(&[p.ca.certificate()], Some(&rogue)).unwrap();
        let (server, _) = handshake(server_cfg, client_cfg).await;
        assert!(server.is_err());
    }

    #[tokio::test]
    async fn untrusted_server_is_rejected_by_device() {
        let p = pki();
        let rogue_ca = DevCa::generate("rogue CA").unwrap();
        let rogue_server = rogue_ca.issue_server(&["localhost"]).unwrap();
        let server_cfg = mutual_server_config(&rogue_server, &[p.ca.certificate()]).unwrap();
        let client_cfg = client_config(&[p.ca.certificate()], Some(&p.device)).unwrap();
        let (server, client) = handshake(server_cfg, client_cfg).await;
        assert!(client.is_err());
        assert!(server.is_err());
    }

    #[tokio::test]
    async fn plaintext_client_is_rejected() {
        let p = pki();
        let server_cfg = mutual_server_config(&p.server, &[p.ca.certificate()]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            TlsAcceptor::from(server_cfg).accept(tcp).await
        });
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(br#"{"type":"door_sensor","door":1,"open":true}"#)
            .await
            .unwrap();
        assert!(accept.await.unwrap().is_err());
    }

    #[test]
    fn identities_round_trip_through_pem_files() {
        let p = pki();
        let dir = std::env::temp_dir().join(format!("envmon-pem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ca_path = dir.join("ca.pem");
        std::fs::write(&ca_path, p.ca.certificate_pem()).unwrap();
        assert_eq!(
            load_certificates(&ca_path).unwrap(),
            vec![p.ca.certificate()]
        );
        std::fs::write(dir.join("empty.pem"), "").unwrap();
        assert!(matches!(
            load_certificates(dir.join("empty.pem")),
            Err(TlsError::NoCertificate)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
