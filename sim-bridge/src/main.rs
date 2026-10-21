use sim_bridge::{config::Config, router};
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() {
    // Run from the directory holding the local `.env`, or point ADSB_SIM_ENV_FILE at it.
    let env_file =
        std::env::var_os("ADSB_SIM_ENV_FILE").map_or_else(|| PathBuf::from(".env"), PathBuf::from);
    let text = std::fs::read_to_string(&env_file).ok();
    let cfg =
        Config::from_sources(text.as_deref(), |k| std::env::var(k).ok()).unwrap_or_else(|e| {
            eprintln!("sim-bridge: {e}");
            std::process::exit(2);
        });
    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .unwrap_or_else(|e| {
            eprintln!("sim-bridge: cannot bind: {e}");
            std::process::exit(1);
        });
    eprintln!(
        "sim-bridge listening on {} (env file {}, feed {}, config {}, capture {})",
        cfg.listen,
        if text.is_some() { "read" } else { "absent" },
        if cfg.feed.is_some() { "set" } else { "unset" },
        if cfg.config_json.is_some() {
            "set"
        } else {
            "unset"
        },
        if cfg.capture.is_some() {
            "set"
        } else {
            "unset"
        },
    );
    axum::serve(listener, router(Arc::new(cfg))).await.unwrap();
}
