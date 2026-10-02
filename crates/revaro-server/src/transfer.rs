//! HTTP body lifetimes and transfer admission.

use axum::body::Body;
use futures_util::StreamExt as _;
use tokio::sync::OwnedSemaphorePermit;

/// Hold admission while bytes are consumed, releasing on EOF or disconnect.
pub fn hold_permit(body: Body, permit: OwnedSemaphorePermit) -> Body {
    Body::from_stream(futures_util::stream::unfold(
        (body.into_data_stream(), permit),
        |(mut stream, permit)| async move { stream.next().await.map(|chunk| (chunk, (stream, permit))) },
    ))
}
