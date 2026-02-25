use axum::{extract::Request, middleware::Next, response::Response};
use uuid::Uuid;

/// Middleware that generates a unique request ID for each API request
/// and adds it to the tracing span for distributed tracing.
pub async fn request_id_middleware(req: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();

    // Create a tracing span with the request ID
    let span = tracing::info_span!(
        "api_request",
        request_id = %request_id,
        method = %req.method(),
        path = %req.uri().path(),
    );

    let _guard = span.enter();
    drop(_guard); // release the span guard before async

    // Add request ID as a response header
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert("x-request-id", request_id.parse().unwrap());

    response
}
