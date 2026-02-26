use tracing_subscriber::{EnvFilter, fmt};

pub fn init_tracing(service_name: &str) {
    init_tracing_with_filter(service_name, "info");
}

pub fn init_tracing_with_filter(service_name: &str, default_filter: &str) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));

    let _ = fmt()
        .with_target(false)
        .with_env_filter(env_filter)
        .compact()
        .try_init();

    tracing::info!(service = service_name, "tracing initialized");
}
