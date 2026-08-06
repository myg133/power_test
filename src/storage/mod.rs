//! Persistence of run artifacts.

pub mod history;

pub use history::{
    ensure_history_dir, list_runs, list_runs_by_model, list_runs_by_target, load_compare_data,
    load_run, run_dir, save_run,
};
