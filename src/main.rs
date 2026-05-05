use std::net::SocketAddr;

use cbsa::{config::AppConfig, db, web};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().json())
        .init();

    let cfg = AppConfig::load()?;
    let pool = db::connect(&cfg).await?;
    db::migrate(&pool).await?;

    let app = web::router(web::AppState { pool: pool.clone() });
    let addr: SocketAddr = cfg.server.bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "cbsa listening");
    axum::serve(listener, app).await?;
    Ok(())
}
