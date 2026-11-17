//! HTTPS (TLS 1.3 only) server; plaintext HTTP is never served.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;

/// Maximum simultaneous connections; further clients wait in the listen backlog.
pub const MAX_CONNECTIONS: usize = 512;
/// Connections that have not completed the TLS handshake by then are dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves `app` on `listener` until the listener fails. Connections that do not complete a
/// TLS handshake within [`HANDSHAKE_TIMEOUT`] are dropped.
pub async fn serve_https(
    listener: TcpListener,
    tls: Arc<ServerConfig>,
    app: Router,
) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(tls);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
            return Ok(());
        };
        let (tcp, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(stream)) =
                tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await
            else {
                return;
            };
            // Per-connection errors (client reset etc.) only affect that client.
            let _ = auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
