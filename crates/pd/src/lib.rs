#![no_std]

//! Iasis power policy: the brick input contract, the charging pool shared by
//! the five target ports and the fixed supplies each port offers.
//!
//! USB Power Delivery messages and their state machines come from
//! [`usbpd`]; this crate decides what Iasis advertises and accepts.

pub mod apple;
pub mod brick;
pub mod power;
mod source;

pub use source::{
    Contract, RequestError, capabilities, fixed_contract, fixed_offers, host_offers,
    nominal_limit_ma, voltage_window,
};
