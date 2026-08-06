//! These types are used to allow a given warp filter to reject a request.  The rejections are
//! handled in a subsequent function, where they are converted into meaningful responses.
//!
//! Rather than being returned to the client as a JSON-RPC response with the `error` field set,
//! they instead indicate a response at the HTTP level only.

use std::fmt::{self, Display, Formatter};

use warp::reject::Reject;

/// Indicates the HTTP request body is greater than the maximum allowed.
///
/// Wraps the configured maximum allowed on the server, set via the `max_body_bytes` parameter in
/// `base_filter()`.
#[derive(Debug)]
pub(crate) struct BodyTooLarge(pub(crate) u64);

impl Display for BodyTooLarge {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(
            formatter,
            "The request payload exceeds the maximum allowed of {} bytes",
            self.0
        )
    }
}

impl Reject for BodyTooLarge {}
