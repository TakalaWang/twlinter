use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

pub fn init(default_level: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let _ = tracing::subscriber::set_global_default(subscriber);
}
