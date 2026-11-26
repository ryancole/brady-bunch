//! brady-bunch's host logic, shared by `brady-bunch.exe` (the host window) and
//! `brady-ctl.exe` (the command-line control client).

pub mod child;
pub mod config;
pub mod control;
pub mod ctrlc;
pub mod events;
pub mod layout;
pub mod ready;
