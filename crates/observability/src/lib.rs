use tracing_subscriber::{EnvFilter, fmt};

pub fn init_tracing(service_name: &str) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = fmt()
        .with_target(false)
        .with_env_filter(env_filter)
        .compact()
        .try_init();

    tracing::info!(service = service_name, "tracing initialized");
}
