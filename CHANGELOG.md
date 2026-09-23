# Changelog

All notable changes to `tsk` are documented in this file.

## [0.1.0-alpha.1] - 2026-09-23

First alpha/testing release. This entire release — code, tests, and
documentation — was built by AI coding agents (Claude Code) under human
direction. Expect bugs; back up your data before relying on it.

### Added

- Vim-native, keyboard-only kanban board with three fixed columns (ToDo /
  Doing / Done), counts in the column titles, and no mouse support anywhere.
- Local SQLite storage, configurable database directory and config file
  path via CLI flag, environment variable, config file, or XDG default.
- Multiple boards: switch, create, rename, and delete from a board
  switcher (`b` / `<Space>b`) or the `:board` command family.
- Password-protected ("locked") boards: each is its own SQLCipher database
  file, keyed with an Argon2id-derived key from a random per-board salt;
  the plaintext main database stores only a locked board's name, path, and
  salt, never its content.
- Full task editing: forms, an external editor for the task body (`o`),
  undo/redo, tags (create/rename/delete/toggle, including a form-scoped tag
  picker), priority levels, start dates, and deadlines.
- Cards coloured by deadline rather than status (status is already shown by
  the column), with a right-aligned deadline badge; all six colours are
  configurable.
- ToDo/Doing columns sort automatically by deadline (most urgent first);
  Done sorts by completion time, newest first; manual reorder (`J`/`K`)
  only swaps cards sharing the same sort key.
- Auto-archive: Done cards older than 5 days leave the board (retained in
  the database) and are reachable through a filterable archive browser
  (`A`).
- Selected-card body preview: the highlighted card expands to show up to
  three word-wrapped lines of its body.
- Notes pane with quick capture (`c`) and promotion of a note to a task
  (`gp`).
- Live search (`/`) over title, body, and tags, with `n`/`N` match
  navigation.
- Help popup (`?`) generated from the same binding table that drives key
  dispatch, so it can't drift from the real keybindings; arrow-key
  equivalents of `hjkl` are listed there.
- Dropbox/Syncthing-safe storage: `journal_mode = "delete"` keeps each
  database a single file between writes, and `tsk` detects the database
  file changing on disk (e.g. a sync client replacing it) and reloads
  automatically.
- Editor resolution that prefers `nvim` over `$VISUAL`/`$EDITOR` when
  no `editor` is set in the config, so an unset environment doesn't
  silently fall back to `vi`/`nano`.
- Inside tmux, the external editor opens as a titled `display-popup` over
  the still-drawn board instead of suspending to a blank screen.
- `install.sh`: builds and installs the release binary, checks build
  dependencies, and warns about `PATH` and tmux truecolor configuration.
- Single binary with SQLCipher and OpenSSL vendored and statically linked,
  so it has no system OpenSSL/SQLite dependency.

### Known issues

- Only Debian 13 has been tested. Linux Mint (MATE) and other distributions
  are unverified, and a binary built on a newer glibc may not run on an
  older one; build natively with `./install.sh` on each machine.
- If every board is locked, `tsk` will not start: there is no passphrase
  prompt before the board is shown. Keep at least one unlocked board.
- A Dropbox "conflicted copy" produced by editing offline on two machines
  at once is not detected or merged; avoid concurrent offline edits and
  quit with `ZZ` before switching machines.
- Editing a task and changing its status in the same form pushes two
  undo entries instead of one; undoing needs two steps to fully revert it.
- Undo/redo history is in-memory for the current session only and does not
  survive a restart.
- The archive browser's live filter can't contain the letters `j`/`k`,
  since those keys navigate rather than type.
