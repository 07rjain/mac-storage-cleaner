//! Sentry error reporting with path scrubbing and a runtime opt-out.

mod scrub;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use sentry::integrations::tracing::layer as tracing_layer;

static ENABLED: AtomicBool = AtomicBool::new(false);

pub struct Config {
    /// Sentry DSN baked in at build time. `None` or empty disables reporting entirely.
    pub dsn: Option<&'static str>,
    /// Release name, `<app-id>@<version>`, matching the changelog version.
    pub release: String,
    /// The user's crash-report preference at launch.
    pub enabled: bool,
}

/// Keeps the Sentry client alive. Dropping it flushes pending events.
pub struct Telemetry {
    _guard: Option<sentry::ClientInitGuard>,
}

pub fn init(config: Config) -> Telemetry {
    ENABLED.store(config.enabled, Ordering::Relaxed);

    let Some(dsn) = config.dsn.filter(|dsn| !dsn.trim().is_empty()) else {
        return Telemetry { _guard: None };
    };

    let mut options = sentry::ClientOptions::default();
    options.release = Some(config.release.into());
    options.environment = Some(environment().into());
    options.send_default_pii = false;
    options.before_send = Some(Arc::new(|event| {
        if is_enabled() {
            scrub::event(event)
        } else {
            None
        }
    }));
    options.before_breadcrumb = Some(Arc::new(|breadcrumb| {
        if is_enabled() {
            scrub::breadcrumb(breadcrumb)
        } else {
            None
        }
    }));

    let guard = sentry::init((dsn, options));

    Telemetry {
        _guard: Some(guard),
    }
}

/// Takes effect for every event captured after this call.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Sends pending events. Call before the process exits through a path that skips destructors.
pub fn flush(timeout: Duration) {
    if let Some(client) = sentry::Hub::current().client() {
        client.flush(Some(timeout));
    }
}

fn environment() -> &'static str {
    if cfg!(debug_assertions) {
        "development"
    } else {
        "production"
    }
}
