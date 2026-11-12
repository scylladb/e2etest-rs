use std::sync::Once;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

static INIT_TRACING: Once = Once::new();

pub(crate) fn enable_tracing() {
    INIT_TRACING.call_once(|| {
        tracing_subscriber::registry()
            .with(EnvFilter::try_new("info").unwrap())
            .with(fmt::layer().with_test_writer().with_target(false))
            .init();
    });
}
