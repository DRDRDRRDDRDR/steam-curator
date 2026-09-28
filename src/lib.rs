//! steam-curator 的核心库。
//!
//! 命令行版（`src/main.rs`）与图形界面版（`src/bin/gui.rs`）共用这里的全部逻辑，
//! 两边只在「怎么把结果呈现给用户」上不同。

#![allow(dead_code)]

pub mod apply;
pub mod model;
pub mod plan;
pub mod preview;
pub mod prompt;
pub mod scan;
pub mod session;
pub mod steam;
pub mod util;
pub mod vdf;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
