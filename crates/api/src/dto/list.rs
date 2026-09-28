use serde::Serialize;

use application::use_cases::list_cruises::ListCruisesResult;

#[derive(Debug, Serialize)]
pub struct ListCruisesResponse {
    pub items: Vec<CruiseListItemDto>,
    pub limit: i64,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CruiseListItemDto {
    pub cruise_id: String,
    pub name: String,
    pub ship_name: Option<String>,
    pub begin_date: String,
    pub end_date: String,
    pub days: Option<i32>,
    pub route: Option<String>,
    pub departure_city: Option<String>,
    pub minimal_price: Option<String>,
    pub room_counts: i32,
    pub is_active: bool,
}

impl From<ListCruisesResult> for ListCruisesResponse {
    fn from(r: ListCruisesResult) -> Self {
        Self {
            items: r
                .items
                .into_iter()
                .map(|c| CruiseListItemDto {
                    cruise_id: c.cruise_id,
                    name: c.name,
                    ship_name: c.ship_name,
                    begin_date: c.begin_date.to_string(),
                    end_date: c.end_date.to_string(),
                    days: c.days,
                    route: c.route,
                    departure_city: c.departure_city,
                    minimal_price: c.minimal_price.map(|d| d.to_string()),
                    room_counts: c.room_counts,
                    is_active: c.is_active,
                })
                .collect(),
            limit: r.limit,
            has_more: r.has_more,
            next_cursor: r.next_cursor.map(|c| c.encode()),
        }
    }
}
