mod ble;
mod db;
mod event;
mod gateway;
mod http;
mod render;
mod sprites;
mod state;

use clap::Parser;
use tracing::info;

#[derive(Parser)]
#[command(name = "lfg", about = "iDotMatrix Raid Frames — Rust edition")]
struct Args {
    /// HTTP server port
    #[arg(short, long, default_value = "5555")]
    port: u16,

    /// Bind address (default: 127.0.0.1 — localhost only)
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,

    /// Optional auth token — if set, all requests must supply `Authorization: Bearer <TOKEN>`
    #[arg(long)]
    token: Option<String>,

    /// BLE device address (e.g. AA:BB:CC:DD:EE:FF). Auto-discovers if omitted.
    #[arg(short, long)]
    device: Option<String>,

    /// Disable BLE (HTTP-only mode for testing)
    #[arg(long)]
    no_ble: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lfg=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();

    let conn = db::open_db("lfg.db");
    let (db_tool_calls, db_unique_agents, db_agent_minutes) = db::load_stats(&conn);
    if db_tool_calls > 0 || db_unique_agents > 0 || db_agent_minutes > 0.0 {
        info!(
            "Loaded persisted stats: tools={}, agents={}, minutes={:.1}",
            db_tool_calls, db_unique_agents, db_agent_minutes
        );
    }

    let shared = state::new_shared_state();
    {
        let mut s = shared.write().await;
        s.stats.tool_calls = db_tool_calls;
        s.stats.unique_agents_count = db_unique_agents;
        s.stats.agent_minutes = db_agent_minutes;
        s.db_conn = Some(std::sync::Mutex::new(conn));
    }

    info!("iDotMatrix 64x64 raid frames (Rust) — 5 columns, 2 agents each");
    info!(
        "Device: {}",
        args.device.as_deref().unwrap_or("auto-discover IDM-*")
    );
    info!("Webhook: http://{}:{}/webhook", args.bind, args.port);
    info!("");
    info!("Status:   curl -s localhost:{}/status", args.port);
    info!("Hosts:    curl -s localhost:{}/hosts", args.port);
    info!("Reset:    curl -sX POST localhost:{}/reset", args.port);
    info!("Themes:   curl -s localhost:{}/theme", args.port);
    info!("Set:      curl -sX POST localhost:{}/theme/0", args.port);

    // BLE render loop
    if !args.no_ble {
        let ble_state = shared.clone();
        tokio::spawn(async move {
            ble::ble_loop(ble_state).await;
        });
    }

    // HTTP server
    let app = http::router(shared, args.token);
    let addr: std::net::SocketAddr = format!("{}:{}", args.bind, args.port)
        .parse()
        .expect("Invalid bind address");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    info!("Listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}
