//! API-крейт как библиотека.
//!
//! Зачем: main.rs — это binary, из него нельзя использовать код в
//! integration-тестах (`crates/api/tests/`). Выносим всё, что должно
//! быть доступно тестам, в lib.

pub mod bootstrap;
pub mod config;
pub mod dto;
pub mod error;
pub mod middleware;
pub mod routes;
pub mod state;
