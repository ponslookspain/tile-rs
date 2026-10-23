//! tile-rs: a tiling window manager for Windows.
#![windows_subsystem = "windows"]

mod app;
mod config;
mod diagnostics;
mod layout;
mod platform;
mod ui;

fn main() {
    app::run();
}
