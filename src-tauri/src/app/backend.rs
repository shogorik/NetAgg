//! Where the popup and the tray find the engine.

use std::sync::Arc;

use serde_json::Value;

use super::control::{self, Caller, Request};
use super::state::AppState;
use super::status;

pub enum Backend {
    /// In this process (macOS), which asks for the rights it needs.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Local(Arc<AppState>),
    /// In the NetAgg service (Windows, Linux); this process runs as the user.
    #[cfg(not(target_os = "macos"))]
    Service,
}

impl Backend {
    pub async fn call(&self, request: Request) -> Result<Value, String> {
        match self {
            // No boundary to guard inside one process.
            Backend::Local(state) => {
                control::handle(state, request, Caller { elevated: true }).await
            }
            #[cfg(not(target_os = "macos"))]
            Backend::Service => super::service::call(request).await,
        }
    }

    /// The status, or one saying why there's none.
    pub async fn status(&self) -> Value {
        match self.call(Request::Status).await {
            Ok(status) => status,
            Err(err) => serde_json::to_value(status::offline(err)).unwrap_or_default(),
        }
    }

    pub fn local(&self) -> Option<&Arc<AppState>> {
        match self {
            Backend::Local(state) => Some(state),
            #[cfg(not(target_os = "macos"))]
            Backend::Service => None,
        }
    }
}
