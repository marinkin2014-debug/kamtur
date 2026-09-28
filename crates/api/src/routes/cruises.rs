use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::NaiveDate;
use serde::Deserialize;

use domain::views::{CruiseListCursor, CruiseListFilter};

use crate::dto::detail::CruiseDetailResponse;
use crate::dto::list::ListCruisesResponse;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub cruise_provider_id: Option<String>,
    pub begin_from: Option<String>,
    pub begin_to: Option<String>,
    pub departure_city: Option<String>,
    pub limit: Option<i64>,
    /// Opaque-курсор из `next_cursor` предыдущего ответа.
    /// Клиент должен передавать его как есть, не парся.
    pub cursor: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/cruises", get(list))
        .route("/cruises/:id", get(get_one))
}

async fn list(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListCruisesResponse>, ApiError> {
    let cursor = match q.cursor.as_deref() {
        None => None,
        Some(s) => Some(
            CruiseListCursor::decode(s)
                .map_err(|e| ApiError::BadRequest(format!("invalid cursor: {e}")))?,
        ),
    };

    let provider = q
        .cruise_provider_id
        .unwrap_or_else(|| state.default_provider_id.to_string());

    let filter = CruiseListFilter {
        cruise_provider_id: Some(provider),
        begin_from: parse_date(&q.begin_from)?,
        begin_to: parse_date(&q.begin_to)?,
        departure_city: q.departure_city,
        limit: q.limit.unwrap_or(20),
        cursor,
    };

    let result = state.list_cruises.execute(filter).await?;
    Ok(Json(result.into()))
}

async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ProviderQuery>,
) -> Result<Json<CruiseDetailResponse>, ApiError> {
    let provider = q
        .cruise_provider_id
        .unwrap_or_else(|| state.default_provider_id.to_string());

    state
        .get_cruise
        .execute(&provider, &id)
        .await?
        .map(|c| Json(c.into()))
        .ok_or_else(|| ApiError::NotFound(format!("cruise {id} not found")))
}

#[derive(Debug, Deserialize)]
pub struct ProviderQuery {
    pub cruise_provider_id: Option<String>,
}

fn parse_date(s: &Option<String>) -> Result<Option<NaiveDate>, ApiError> {
    match s {
        None => Ok(None),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| ApiError::BadRequest(format!("invalid date: {s}, expected YYYY-MM-DD"))),
    }
}
