//! Genesis: a server for the EVE Online build-1000 client (May 2003, "Gemini").
//!
//! Everything here is derived from the client itself: the decompiled scripts
//! (decompiled/src) and the native DLLs (decompiled/native). See docs/.

mod accounts;
mod conn;
mod db;
mod registry;
mod rows;
mod services;
mod session;
mod space;
mod statics;
mod world;

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use deadpool_postgres::Pool;
use tokio::net::TcpListener;

pub struct Config {
    pub listen: String,
    pub database_url: String,
    pub node_id: i64,
    pub auto_create_accounts: bool,
    /// Directory holding static/*.sql and game/*.sql.
    pub db_dir: std::path::PathBuf,
}

impl Config {
    fn from_env() -> Config {
        let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        Config {
            // The login screen defaults to port 26000 (ui/login/login.py:241).
            listen: var("GENESIS_LISTEN", "0.0.0.0:26000"),
            database_url: var(
                "DATABASE_URL",
                "postgres://genesis:genesis@localhost:5432/genesis",
            ),
            node_id: var("GENESIS_NODE_ID", "1").parse().unwrap_or(1),
            auto_create_accounts: var("GENESIS_AUTO_CREATE_ACCOUNTS", "1") == "1",
            db_dir: var("GENESIS_DB_DIR", "db").into(),
        }
    }
}

pub struct Server {
    pub config: Config,
    pub pool: Pool,
    pub statics: statics::Statics,
    pub world: world::World,
    pub registry: registry::Registry,
    pub space: space::Space,
    pub gangs: services::gang::Gangs,
    pub chat: services::chat::Channels,
    pub combat: services::combat::Combat,
    pub trades: services::trade::Trades,
    online: AtomicUsize,
}

impl Server {
    pub fn user_count(&self) -> i32 {
        self.online.load(Ordering::Relaxed) as i32
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_env();
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool, &config.db_dir).await?;
    let world = world::World::load(&pool).await.context("loading world")?;
    let statics = statics::Statics::load(&pool)
        .await
        .context("loading static data")?;
    for t in &statics.tables {
        // Each cached object travels in one CallRsp frame, which the client
        // caps at 1,000,000 bytes; leave headroom for the packet envelope.
        anyhow::ensure!(
            t.pickle.len() < proto::framing::MAX_FRAME - 4096,
            "{} is {} bytes marshalled, over the client's frame limit",
            t.key,
            t.pickle.len()
        );
        tracing::info!(
            key = t.key,
            rows = t.rows,
            bytes = t.pickle.len(),
            "static table"
        );
    }

    let listener = TcpListener::bind(&config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!("Genesis listening on {}", config.listen);
    let server = Arc::new(Server {
        config,
        pool,
        statics,
        world,
        registry: registry::Registry::default(),
        space: space::Space::default(),
        gangs: Default::default(),
        chat: Default::default(),
        combat: Default::default(),
        trades: Default::default(),
        online: AtomicUsize::new(0),
    });
    tokio::spawn(services::space::simulate(server.clone()));
    tokio::spawn(services::skills::run_ticker(server.clone()));
    let next_client = AtomicI64::new(1);
    loop {
        let (stream, peer) = listener.accept().await?;
        stream.set_nodelay(true).ok();
        let server = server.clone();
        let client_id = next_client.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            tracing::info!(%peer, client_id, "connected");
            server.online.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = conn::serve(server.clone(), stream, peer, client_id).await {
                tracing::warn!(%peer, "connection ended: {e:#}");
            }
            server.online.fetch_sub(1, Ordering::Relaxed);
        });
    }
}
