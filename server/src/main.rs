use kodex_relay_server::{
    config::Config, db::Db, errors::Result, health, state::AppState, subscription, transport,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();
    let config = Config::from_env();
    tracing::info!(?config, "starting kodex-relay-server");
    let db = Db::open(&config.db_path)?;
    let health_addr = config.health_addr;
    let state = AppState::new(config, db);

    // Background tasks: periodic subscription-expiry sweeper + health probe.
    tokio::spawn(subscription::run_sweeper(state.clone()));
    tokio::spawn(health::run(health_addr));

    tokio::select! {
        res = transport::run(state) => {
            if let Err(e) = res {
                tracing::error!(error = %e, "server exited with error");
                return Err(e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c received; shutting down");
        }
    }
    Ok(())
}
