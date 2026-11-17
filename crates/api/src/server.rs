//! HTTPS (TLS 1.3 only) server; plaintext HTTP is never served.

use std::sync::Arc;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;

/// Serves `app` on `listener` until the listener fails. Connections that do not complete a
/// TLS handshake are dropped.
pub async fn serve_https(
    listener: TcpListener,
    tls: Arc<ServerConfig>,
    app: Router,
) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(tls);
    loop {
        let (tcp, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(tcp).await else {
                return;
            };
            // Per-connection errors (client reset etc.) only affect that client.
            let _ = auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
