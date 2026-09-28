use rust_decimal::Decimal;

#[derive(Debug, Default)]
pub struct RawData {
    pub ships: Vec<RawShip>,
    pub decks: Vec<RawDeck>,
    pub classes: Vec<RawClass>,
    pub cabins: Vec<RawCabin>,
    pub cruises: Vec<RawCruise>,
    pub prices: Vec<RawPrice>,
    pub spos: Vec<RawSpo>,
    pub free: Vec<RawFree>,
}

#[derive(Debug)]
pub struct RawShip {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct RawDeck {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct RawClass {
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    pub m_count: Option<i32>,
    pub r_count: Option<i32>,
    pub no_full: Option<bool>,
}

#[derive(Debug)]
pub struct RawCabin {
    pub id: String,
    pub ship: String,
    pub number: String,
    pub class_id: String,
    pub deck: String,
}

#[derive(Debug)]
pub struct RawCruise {
    pub id: String,
    pub ship_id: String,
    pub begin_date: String,
    pub begin_time: Option<String>,
    pub end_date: String,
    pub end_time: Option<String>,
    pub route: String,
    pub child_price: Option<Decimal>,
    pub dop_price: Option<Decimal>,
}

#[derive(Debug)]
pub struct RawPrice {
    pub cruise_id: String,
    pub class_id: String,
    pub price: Decimal,
    pub nofull: Option<bool>,
}

#[derive(Debug)]
pub struct RawSpo {
    pub cruise_id: String,
    pub class_id: String,
    pub cabin_id: String,
    pub spo: Decimal,
    pub nofull: Option<bool>,
}

#[derive(Debug)]
pub struct RawFree {
    pub cruise_id: String,
    pub cabin_id: String,
}
