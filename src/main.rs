mod app;
mod assets;
mod blocks;
mod camera;
mod commands;
mod config;
mod console;
mod input;
mod meshing;
pub mod perf;
mod player;
mod registry;
mod render;
mod settings;
mod sim;
mod storage;
mod texture;
mod texture_registry;
mod util;
mod world;
mod worldgen;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|arg| arg == "--migrate-save-v1-to-v2") {
        let report = storage::migrate_save_v1_to_v2(std::path::Path::new("save"))?;
        crate::console_info!(
            "save migration completed: scanned={} migrated={} skipped_non_v1={}",
            report.scanned_columns,
            report.migrated_columns,
            report.skipped_non_v1_columns
        );
        return Ok(());
    }

    let _ = perf::trace::trace_init();
    assets::ensure_resource_pack_dir();
    let result = app::run();
    perf::trace::trace_shutdown();
    result
}
