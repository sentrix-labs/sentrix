// types.rs — REST response + request DTOs shared across handlers.
// Pulled out of the monolithic `routes.rs` during the backlog #12
// refactor.

use axum::Json;
use sentrix_primitives::transaction::Transaction;
use serde::{Deserialize, Serialize};

/// Generic REST response wrapper used by handlers that don't return a
/// bespoke JSON shape. `data` carries the success payload; `error`
/// carries a human-readable failure message. Callers use
/// `ApiResponse::ok(data)` and `ApiResponse::err(msg)` constructors.
#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Json<Self> {
        Json(Self {
            success: true,
            data: Some(data),
            error: None,
        })
    }

    pub fn err(msg: String) -> Json<ApiResponse<()>> {
        Json(ApiResponse {
            success: false,
            data: None,
            error: Some(msg),
        })
    }
}

/// Request body for the generic `POST /transactions` endpoint — a single
/// pre-signed transaction.
#[derive(Deserialize)]
pub struct SendTxRequest {
    pub transaction: Transaction,
}

/// Request body for the token endpoints
/// (`POST /tokens/deploy | /tokens/{c}/transfer | /tokens/{c}/burn`).
/// Token endpoints accept pre-signed transactions only — private keys
/// stay client-side. The client builds a `TokenOp` JSON, encodes it in
/// `tx.data`, signs the transaction locally, then POSTs the signed
/// object here.
#[derive(Deserialize)]
pub struct SignedTxRequest {
    pub transaction: Transaction,
}
