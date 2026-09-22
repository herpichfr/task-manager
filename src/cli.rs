//! Command-line argument definitions.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    /// Path to the config file, overriding the default location.
    #[arg(short = 'c', long = "config", value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Directory holding the task database, overriding the default location.
    #[arg(short = 'd', long = "db-dir", value_name = "DIR")]
    pub db_dir: Option<PathBuf>,

    /// Board to open on startup.
    #[arg(short = 'b', long = "board", value_name = "NAME")]
    pub board: Option<String>,
}
