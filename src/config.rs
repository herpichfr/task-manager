//! Configuration file model and path/editor resolution.

use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub db_dir: Option<PathBuf>,
    pub default_board: Option<String>,
    pub editor: Option<String>,
    pub lock_on_board_switch: bool,
    pub confirm_delete: bool,
    pub show_keyhints: bool,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            db_dir: None,
            default_board: None,
            editor: None,
            lock_on_board_switch: true,
            confirm_delete: true,
            show_keyhints: true,
            theme: Theme::default(),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub todo: String,
    pub doing: String,
    pub done: String,
    pub priority_low: String,
    pub priority_normal: String,
    pub priority_high: String,
    pub priority_urgent: String,
    pub border: String,
    pub selection: String,
    pub keyhints: String,
    /// Card colours by deadline urgency. A card's colour comes from its
    /// deadline, not its status -- status is shown by which column it is in.
    /// Names ("red"), or `#rrggbb`, which degrades automatically on
    /// terminals without truecolor.
    pub deadline_none: String,
    pub deadline_distant: String,
    pub deadline_soon: String,
    pub deadline_near: String,
    pub deadline_imminent: String,
    pub deadline_overdue_fg: String,
    pub deadline_overdue_bg: String,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            todo: "yellow".to_string(),
            doing: "blue".to_string(),
            done: "green".to_string(),
            priority_low: "darkgray".to_string(),
            priority_normal: "white".to_string(),
            priority_high: "magenta".to_string(),
            priority_urgent: "red".to_string(),
            border: "darkgray".to_string(),
            selection: "reverse".to_string(),
            keyhints: "darkgray".to_string(),
            // no due date
            deadline_none: "darkgray".to_string(),
            // more than 15 days out
            deadline_distant: "green".to_string(),
            // 14-5 days
            deadline_soon: "yellow".to_string(),
            // 4-2 days: orange. Written as hex so it degrades to the
            // nearest 256- or 16-colour automatically; there is no ANSI
            // named orange.
            deadline_near: "#ff8700".to_string(),
            // less than 2 days
            deadline_imminent: "red".to_string(),
            // overdue: white on black
            deadline_overdue_fg: "white".to_string(),
            deadline_overdue_bg: "black".to_string(),
        }
    }
}

/// Resolved filesystem locations for this run.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub db_dir: PathBuf,
}

/// Pure resolver. Precedence, highest first: CLI flag, env value, config-file
/// value, XDG default. The config file itself has no config-file-value level
/// (there being no config loaded yet to supply one), so it resolves from
/// CLI, env, then the XDG default.
pub fn resolve_paths(
    cli_config: Option<&Path>,
    env_config: Option<&Path>,
    cli_db_dir: Option<&Path>,
    env_db_dir: Option<&Path>,
    config_db_dir: Option<&Path>,
    xdg_config_default: &Path,
    xdg_data_default: &Path,
) -> Paths {
    let config_file = cli_config
        .or(env_config)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| xdg_config_default.to_path_buf());

    let db_dir = cli_db_dir
        .or(env_db_dir)
        .or(config_db_dir)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| xdg_data_default.to_path_buf());

    Paths { config_file, db_dir }
}

/// Expands a leading `~` (whether bare or followed by `/...`) using the
/// supplied home directory. Any other path is returned unchanged.
pub fn expand_tilde(p: &Path, home: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) if rest.as_os_str().is_empty() => home.to_path_buf(),
        Ok(rest) => home.join(rest),
        Err(_) => p.to_path_buf(),
    }
}

/// Reads and parses a config file. A missing file yields `Config::default()`;
/// any other read failure or a parse failure is propagated as an error.
pub fn load_from_path(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(AppError::Io(e)),
    }
}

/// XDG defaults via `directories::ProjectDirs::from("", "", "tsk")`:
/// config `~/.config/tsk/config.toml`, data `~/.local/share/tsk`.
pub fn xdg_defaults() -> Result<(PathBuf, PathBuf)> {
    let dirs = directories::ProjectDirs::from("", "", "tsk").ok_or(AppError::NoHomeDirectory)?;
    let config_file = dirs.config_dir().join("config.toml");
    let data_dir = dirs.data_dir().to_path_buf();
    Ok((config_file, data_dir))
}

/// Order: explicit `config.editor` (if set and non-empty) -> "nvim" if on
/// PATH -> $VISUAL -> $EDITOR -> "vim" if on PATH -> Err(EditorNotFound).
/// `nvim` outranks $VISUAL and $EDITOR by design: on these machines $EDITOR
/// is usually unset, and deferring to it silently lands the user on vi/nano.
pub fn resolve_editor(
    cfg: &Config,
    lookup: &dyn Fn(&str) -> bool,
    visual: Option<&str>,
    editor: Option<&str>,
) -> Result<String> {
    if let Some(e) = cfg.editor.as_deref().filter(|e| !e.is_empty()) {
        return Ok(e.to_string());
    }
    if lookup("nvim") {
        return Ok("nvim".to_string());
    }
    if let Some(v) = visual.filter(|v| !v.is_empty()) {
        return Ok(v.to_string());
    }
    if let Some(e) = editor.filter(|e| !e.is_empty()) {
        return Ok(e.to_string());
    }
    if lookup("vim") {
        return Ok("vim".to_string());
    }
    Err(AppError::EditorNotFound)
}

/// Production PATH lookup: true if `bin` names an executable file in some
/// directory listed in `$PATH`. Never shells out.
pub fn path_lookup(bin: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| {
        let candidate = dir.join(bin);
        is_executable_file(&candidate)
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- resolve_paths ---------------------------------------------------

    #[test]
    fn resolve_paths_cli_wins_for_config() {
        let paths = resolve_paths(
            Some(Path::new("/cli/config.toml")),
            Some(Path::new("/env/config.toml")),
            None,
            None,
            None,
            Path::new("/xdg/config.toml"),
            Path::new("/xdg/data"),
        );
        assert_eq!(paths.config_file, PathBuf::from("/cli/config.toml"));
    }

    #[test]
    fn resolve_paths_env_wins_for_config_over_xdg() {
        let paths = resolve_paths(
            None,
            Some(Path::new("/env/config.toml")),
            None,
            None,
            None,
            Path::new("/xdg/config.toml"),
            Path::new("/xdg/data"),
        );
        assert_eq!(paths.config_file, PathBuf::from("/env/config.toml"));
    }

    #[test]
    fn resolve_paths_xdg_default_for_config() {
        let paths = resolve_paths(
            None,
            None,
            None,
            None,
            None,
            Path::new("/xdg/config.toml"),
            Path::new("/xdg/data"),
        );
        assert_eq!(paths.config_file, PathBuf::from("/xdg/config.toml"));
    }

    #[test]
    fn resolve_paths_cli_wins_for_db_dir() {
        let paths = resolve_paths(
            None,
            None,
            Some(Path::new("/cli/db")),
            Some(Path::new("/env/db")),
            Some(Path::new("/cfg/db")),
            Path::new("/xdg/config.toml"),
            Path::new("/xdg/data"),
        );
        assert_eq!(paths.db_dir, PathBuf::from("/cli/db"));
    }

    #[test]
    fn resolve_paths_env_wins_for_db_dir_over_config_and_xdg() {
        let paths = resolve_paths(
            None,
            None,
            None,
            Some(Path::new("/env/db")),
            Some(Path::new("/cfg/db")),
            Path::new("/xdg/config.toml"),
            Path::new("/xdg/data"),
        );
        assert_eq!(paths.db_dir, PathBuf::from("/env/db"));
    }

    #[test]
    fn resolve_paths_config_wins_for_db_dir_over_xdg() {
        let paths = resolve_paths(
            None,
            None,
            None,
            None,
            Some(Path::new("/cfg/db")),
            Path::new("/xdg/config.toml"),
            Path::new("/xdg/data"),
        );
        assert_eq!(paths.db_dir, PathBuf::from("/cfg/db"));
    }

    #[test]
    fn resolve_paths_xdg_default_for_db_dir() {
        let paths = resolve_paths(
            None,
            None,
            None,
            None,
            None,
            Path::new("/xdg/config.toml"),
            Path::new("/xdg/data"),
        );
        assert_eq!(paths.db_dir, PathBuf::from("/xdg/data"));
    }

    // --- expand_tilde ------------------------------------------------------

    #[test]
    fn expand_tilde_expands_leading_tilde_slash() {
        let home = Path::new("/home/alice");
        assert_eq!(
            expand_tilde(Path::new("~/x"), home),
            PathBuf::from("/home/alice/x")
        );
    }

    #[test]
    fn expand_tilde_leaves_absolute_path_unchanged() {
        let home = Path::new("/home/alice");
        assert_eq!(
            expand_tilde(Path::new("/abs/x"), home),
            PathBuf::from("/abs/x")
        );
    }

    #[test]
    fn expand_tilde_leaves_relative_path_unchanged() {
        let home = Path::new("/home/alice");
        assert_eq!(
            expand_tilde(Path::new("rel/x"), home),
            PathBuf::from("rel/x")
        );
    }

    #[test]
    fn expand_tilde_expands_bare_tilde() {
        let home = Path::new("/home/alice");
        assert_eq!(expand_tilde(Path::new("~"), home), PathBuf::from("/home/alice"));
    }

    // --- load_from_path ------------------------------------------------------

    #[test]
    fn load_from_path_missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let cfg = load_from_path(&path).unwrap();
        assert_eq!(cfg.default_board, None);
        assert!(cfg.lock_on_board_switch);
        assert!(cfg.confirm_delete);
        assert!(cfg.show_keyhints);
    }

    #[test]
    fn load_from_path_valid_toml_overrides_some_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
default_board = "work"
confirm_delete = false

[theme]
todo = "cyan"
"#,
        )
        .unwrap();
        let cfg = load_from_path(&path).unwrap();
        assert_eq!(cfg.default_board.as_deref(), Some("work"));
        assert!(!cfg.confirm_delete);
        assert!(cfg.lock_on_board_switch);
        assert_eq!(cfg.theme.todo, "cyan");
        assert_eq!(cfg.theme.doing, "blue");
    }

    #[test]
    fn load_from_path_malformed_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not = [valid toml").unwrap();
        assert!(load_from_path(&path).is_err());
    }

    #[test]
    fn load_from_path_unknown_key_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unknown.toml");
        std::fs::write(&path, "not_a_real_key = true").unwrap();
        assert!(load_from_path(&path).is_err());
    }

    // --- resolve_editor ------------------------------------------------------

    #[test]
    fn resolve_editor_config_wins_over_everything() {
        let cfg = Config {
            editor: Some("emacs".to_string()),
            ..Config::default()
        };
        let lookup = |_: &str| true;
        let result = resolve_editor(&cfg, &lookup, Some("code"), Some("nano")).unwrap();
        assert_eq!(result, "emacs");
    }

    #[test]
    fn resolve_editor_nvim_outranks_visual_and_editor() {
        let cfg = Config::default();
        let lookup = |bin: &str| bin == "nvim";
        let result = resolve_editor(&cfg, &lookup, Some("emacs"), Some("nano")).unwrap();
        assert_eq!(result, "nvim");
    }

    #[test]
    fn resolve_editor_visual_used_when_nvim_absent() {
        let cfg = Config::default();
        let lookup = |_: &str| false;
        let result = resolve_editor(&cfg, &lookup, Some("emacs"), Some("nano")).unwrap();
        assert_eq!(result, "emacs");
    }

    #[test]
    fn resolve_editor_editor_used_when_nvim_and_visual_absent() {
        let cfg = Config::default();
        let lookup = |_: &str| false;
        let result = resolve_editor(&cfg, &lookup, None, Some("nano")).unwrap();
        assert_eq!(result, "nano");
    }

    #[test]
    fn resolve_editor_falls_back_to_vim() {
        let cfg = Config::default();
        let lookup = |bin: &str| bin == "vim";
        let result = resolve_editor(&cfg, &lookup, None, None).unwrap();
        assert_eq!(result, "vim");
    }

    #[test]
    fn resolve_editor_errors_when_nothing_available() {
        let cfg = Config::default();
        let lookup = |_: &str| false;
        let result = resolve_editor(&cfg, &lookup, None, None);
        assert!(result.is_err());
    }
}
