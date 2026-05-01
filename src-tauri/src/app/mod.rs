//! Application wiring: Tauri builder setup, lifecycle, shortcuts, fire pipeline.

pub mod context;
pub mod fire;
pub mod lifecycle;
pub mod setup;
pub mod shortcuts;

pub use context::AppContext;
pub use fire::FireService;
