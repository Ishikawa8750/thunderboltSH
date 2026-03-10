mod routes;

use std::net::SocketAddr;

use axum::Router;

use crate::error::OpenBoltResult;

pub async fn spawn_file_api_server(
    bind_ip: String,
    port: u16,
    api_token: Option<String>
) -> OpenBoltResult<tokio::task::JoinHandle<()>> {
    let app = Router::new().merge(routes::router(api_token));

    let addr: SocketAddr = format!("{bind_ip}:{port}")
        .parse::<SocketAddr>()
        .map_err(|e| crate::error::OpenBoltError::CommandFailed(e.to_string()))?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let handle = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("file api stopped: {err}");
        }
    });

    Ok(handle)
}
