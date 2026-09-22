//! `tsk` entry point: resolves configuration, opens the database, and runs
//! the TUI.

use std::path::PathBuf;

use clap::Parser;

use tsk::app::{self, App};
use tsk::cli::Cli;
use tsk::config::{
    expand_tilde, load_from_path, path_lookup, resolve_editor, resolve_paths, xdg_defaults,
};
use tsk::error::{AppError, Result};
use tsk::event;
use tsk::storage::main_db::MainDb;

fn run() -> Result<()> {
    let cli = Cli::parse();

    let env_config = std::env::var("TSK_CONFIG").ok().map(PathBuf::from);
    let env_db_dir = std::env::var("TSK_DB_DIR").ok().map(PathBuf::from);

    let (xdg_config_default, xdg_data_default) = xdg_defaults()?;

    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or(AppError::NoHomeDirectory)?;

    let cli_config = cli.config.as_deref().map(|p| expand_tilde(p, &home));
    let env_config = env_config.as_deref().map(|p| expand_tilde(p, &home));
    let cli_db_dir = cli.db_dir.as_deref().map(|p| expand_tilde(p, &home));
    let env_db_dir = env_db_dir.as_deref().map(|p| expand_tilde(p, &home));

    // Resolve the config file path first (db_dir not yet known), load it,
    // then resolve the final paths using the config file's db_dir value.
    let prelim = resolve_paths(
        cli_config.as_deref(),
        env_config.as_deref(),
        None,
        None,
        None,
        &xdg_config_default,
        &xdg_data_default,
    );

    let config = load_from_path(&prelim.config_file)?;

    let config_db_dir = config.db_dir.as_deref().map(|p| expand_tilde(p, &home));

    let paths = resolve_paths(
        cli_config.as_deref(),
        env_config.as_deref(),
        cli_db_dir.as_deref(),
        env_db_dir.as_deref(),
        config_db_dir.as_deref(),
        &xdg_config_default,
        &xdg_data_default,
    );

    let visual = std::env::var("VISUAL").ok();
    let editor_env = std::env::var("EDITOR").ok();
    let _editor = resolve_editor(&config, &path_lookup, visual.as_deref(), editor_env.as_deref())?;

    let requested_board = cli.board.clone();

    let db = MainDb::open(&paths.db_dir.join("main.db")).map_err(app::storage_err)?;
    let board = app::bootstrap_and_select_board(
        &db,
        requested_board.as_deref(),
        config.default_board.as_deref(),
    )?;
    app::ensure_startable(&board)?;

    let mut app = App::new(config, db, board)?;
    // New locked boards are created next to `main.db`, in the same
    // resolved data directory -- not necessarily `config.db_dir`, which is
    // only the config-*file*'s value and ignores a `--db-dir`/`TSK_DB_DIR`
    // override. `App::new` itself has no access to `paths`, so this is set
    // separately right after construction.
    app.set_data_dir(paths.db_dir.clone());
    event::run(&mut app)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
