// The no-SDL build keeps the editor's shared data and protocol modules
// available, but its `main_loop::run` is intentionally a stub. Private UI
// helpers therefore have no reachable caller in that feature configuration.
#![cfg_attr(not(feature = "sdl"), allow(dead_code))]

pub mod app_state;
pub mod buffer;
pub mod cmdview;
pub mod common;
pub mod config;
pub mod context_menu;
pub mod doc_view;
pub mod draw_context;
pub mod empty_view;
pub mod error;
pub mod event;
pub mod field_history;
pub mod filesystem;
pub mod git;
pub mod keymap;
pub mod log_view;
pub mod lsp;
pub mod lsp_client;
pub mod main_loop;
pub mod markdown;
pub mod markdown_preview;
pub mod nag_view;
pub mod node_view;
pub mod open_doc;
pub(crate) mod perf;
pub mod picker;
#[cfg(unix)]
pub mod process;
pub mod regex;
pub mod status_view;
pub mod storage;
pub mod style;
pub mod style_ctx;
pub mod subsystems;
pub mod syntax;
pub mod terminal;
pub mod terminal_buffer;
pub mod terminal_panel;
#[cfg(windows)]
pub mod terminal_windows;
pub mod test_runner;
pub mod title_view;
pub mod tokenizer;
pub mod toolbar_view;
pub mod tree_view;
pub mod types;
pub mod utf8;
pub mod view;
