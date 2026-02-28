use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::AppState;
// Removed: use std::sync::Arc;
// Removed: use pkg_types::rbac::{Role, RoleBinding, Subject, SubjectKind};

/// Information about the authenticated entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthUser {
    pub name: String,
    pub token: String,
}

/// Middleware: Authenticates the request using a Bearer token.
/// Currently, we validate against the global `join_token`. In a real system,
/// this would look up ServiceAccount tokens or user tokens in SlateDB.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req.headers().get(header::AUTHORIZATION);

    let token = match auth_header {
        Some(value) => {
            let value_str = value.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
            if !value_str.starts_with("Bearer ") {
                return Err(StatusCode::UNAUTHORIZED);
            }
            value_str.trim_start_matches("Bearer ").to_string()
        }
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    // Very simplified auth check:
    // If it's the join token, map it to a cluster-admin identity.
    if token == state.join_token {
        let user = AuthUser {
            name: "admin".to_string(), // For demo purposes
            token,
        };
        // Inject the authenticated user into the request extensions
        req.extensions_mut().insert(user);
        Ok(next.run(req).await)
    } else {
        warn!("Invalid Bearer token provided");
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Extracts the action (verb) from the HTTP method.
fn action_from_method(method: &axum::http::Method) -> &'static str {
    match *method {
        axum::http::Method::GET => "get",
        axum::http::Method::POST => "create",
        axum::http::Method::PUT | axum::http::Method::PATCH => "update",
        axum::http::Method::DELETE => "delete",
        _ => "",
    }
}

/// Simplistic mapping from an API path to a resource type.
fn resource_from_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() >= 4 && parts[1] == "api" && parts[2] == "v1" {
        // e.g. /api/v1/namespaces -> namespaces
        // e.g. /api/v1/namespaces/default/pods -> pods
        if parts.len() >= 6 {
            return parts[5].to_string(); // pods, services, etc.
        } else if parts.len() >= 4 {
            return parts[3].to_string(); // namespaces, nodes
        }
    }
    "*".to_string() // Fallback
}

/// Middleware: Checks if the authenticated user has RBAC permission for the action.
/// (Simplified implementation).
pub async fn rbac_middleware(
    State(_state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let user = req
        .extensions()
        .get::<AuthUser>()
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let path = req.uri().path();
    let method = req.method();
    let action = action_from_method(method);
    let resource = resource_from_path(path);

    info!(
        "RBAC check: user={} action={} resource={} path={}",
        user.name, action, resource, path
    );

    // Hardcode cluster-admin bypass for simplicty, assuming the `admin` uses the join_token.
    // In a full implementation, we would query prefix `/registry/rbac/rolebindings/`
    // and check the rules within referenced `/registry/rbac/roles/`.
    if user.name == "admin" {
        return Ok(next.run(req).await);
    }

    warn!(
        "RBAC denied: user={} action={} resource={}",
        user.name, action, resource
    );
    Err(StatusCode::FORBIDDEN)
}
