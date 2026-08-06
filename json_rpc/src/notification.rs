use serde::Serialize;

use crate::JSON_RPC_VERSION;

/// An outbound JSON-RPC notification.
///
/// Notifications contain a method and parameters but no request ID, and therefore do not expect a
/// response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Notification<'a, P> {
    jsonrpc: &'static str,
    method: &'a str,
    params: P,
}

impl<'a, P> Notification<'a, P> {
    /// Creates a notification for `method` with the supplied parameters.
    #[must_use]
    pub fn new(method: &'a str, params: P) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION,
            method,
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn serializes_without_an_id() {
        let notification = Notification::new("notify", json!({"value": 1}));

        assert_eq!(
            serde_json::to_value(notification).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "method": "notify",
                "params": {"value": 1},
            })
        );
    }
}
