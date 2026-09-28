use serde::Serialize;

use domain::views::CruiseDetail;

#[derive(Debug, Serialize)]
pub struct CruiseDetailResponse {
    pub cruise_id: String,
    pub name: String,
    pub ship_name: Option<String>,
    pub begin_date: String,
    pub begin_time: Option<String>,
    pub end_date: String,
    pub end_time: Option<String>,
    pub days: Option<i32>,
    pub route: Option<String>,
    pub departure_city: Option<String>,
    pub city_from: Option<String>,
    pub city_to: Option<String>,
    pub is_return: Option<bool>,
    pub is_weekend: Option<bool>,
    pub is_active: bool,
    pub status: String,
    pub prices: Vec<ClassPriceDto>,
    pub rooms: Vec<RoomDto>,
}

#[derive(Debug, Serialize)]
pub struct ClassPriceDto {
    pub class_id: String,
    pub class_name: String,
    pub description: Option<String>,
    pub base_seats: Option<i32>,
    pub tiers: Option<i32>,
    pub partial_buyout: bool,
    pub base_price: String,
    pub child_price: Option<String>,
    pub extra_seat: Option<String>,
    pub currency: String,
}

#[derive(Debug, Serialize)]
pub struct RoomDto {
    pub room_id: String,
    pub number: String,
    pub class_id: String,
    pub class_name: String,
    pub stage_id: Option<String>,
    pub stage_name: Option<String>,
    pub available: bool,
    pub prices: Vec<RoomPriceDto>,
}

#[derive(Debug, Serialize)]
pub struct RoomPriceDto {
    pub partial_buyout: bool,
    pub base_price: String,
    pub currency: String,
}

impl From<CruiseDetail> for CruiseDetailResponse {
    fn from(c: CruiseDetail) -> Self {
        Self {
            cruise_id: c.cruise_id,
            name: c.name,
            ship_name: c.ship_name,
            begin_date: c.begin_date.to_string(),
            begin_time: c.begin_time.map(|t| t.to_string()),
            end_date: c.end_date.to_string(),
            end_time: c.end_time.map(|t| t.to_string()),
            days: c.days,
            route: c.route,
            departure_city: c.departure_city,
            city_from: c.city_from,
            city_to: c.city_to,
            is_return: c.is_return,
            is_weekend: c.is_weekend,
            is_active: c.is_active,
            status: c.status,
            prices: c
                .prices
                .into_iter()
                .map(|p| ClassPriceDto {
                    class_id: p.class_id,
                    class_name: p.class_name,
                    description: p.description,
                    base_seats: p.base_seats,
                    tiers: p.tiers,
                    partial_buyout: p.partial_buyout,
                    base_price: p.base_price.to_string(),
                    child_price: p.child_price.map(|d| d.to_string()),
                    extra_seat: p.extra_seat.map(|d| d.to_string()),
                    currency: p.currency,
                })
                .collect(),
            rooms: c
                .rooms
                .into_iter()
                .map(|r| RoomDto {
                    room_id: r.room_id,
                    number: r.number,
                    class_id: r.class_id,
                    class_name: r.class_name,
                    stage_id: r.stage_id,
                    stage_name: r.stage_name,
                    available: r.available,
                    prices: r
                        .prices
                        .into_iter()
                        .map(|p| RoomPriceDto {
                            partial_buyout: p.partial_buyout,
                            base_price: p.base_price.to_string(),
                            currency: p.currency,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}
