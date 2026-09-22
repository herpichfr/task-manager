# HANDOFF — `tsk`

Terminal kanban task manager and quick note taker. Vim-native, keyboard-only,
SQLite-backed, with password-protected boards whose database file is itself
encrypted.

**Status**: all 14 original requirements plus a round of user feedback are done
and verified. 258 tests pass, `cargo clippy --all-targets -- -D warnings` is
clean, ~8,900 lines. Approved plan: `~/.claude/plans/silly-tinkering-perlis.md`.

Installed binary: `~/.local/bin/tsk` (8 MB, release, stripped). Rebuild and
reinstall with `./install.sh`.

---

## How to build and run

```sh
./install.sh                             # build + install to ~/.local/bin
./install.sh --prefix /usr/local         # system-wide (needs sudo)
./install.sh --uninstall                 # remove the binary, keep the data

export PATH="$HOME/.cargo/bin:$PATH"     # rustup toolchain, rustc 1.98.1
cargo build                              # or --release
cargo test                               # 258 tests, all headless
cargo clippy --all-targets -- -D warnings
./target/debug/tsk                       # needs a real TTY
TSK_DB_DIR=/tmp/scratch ./target/debug/tsk   # isolated database
```

There is no `rustc` from apt in use: Debian ships 1.85.1, which is below what
current ratatui/rusqlite expect. The toolchain was installed via `rustup` into
`~/.cargo` with no sudo.

---

## What works

| Requirement | State |
|---|---|
| 1 Lightweight | Single binary, no runtime deps beyond libc (SQLCipher + OpenSSL are statically vendored) |
| 2 Vim keybindings / navigation | Done: modal, counts, two-key sequences (`gg`, `dd`, `gp`, `ZZ`) |
| 3 Local SQLite | Done |
| 4 No mouse | Done — no mouse capture is ever enabled |
| 5 Dropdowns for lists | Done: column, priority, tag, board switcher |
| 6 Tasks coloured | **By deadline, not status** (user's later instruction) — see the colour table below |
| 7 Manual status change | Done: `H`/`L`, or the `m` dropdown |
| 8 Kanban like kanban-tui | Done: three columns, counts in titles |
| 9 Configurable db/config paths | Done: CLI flag → env → config file → XDG |
| 10 Debian-like + tmux | Works on Debian 13 in tmux; **Mint not yet tested** (see Open work) |
| 11 Multiple boards | Done: switcher, `:board new/rename/delete` |
| 12 Password-secured board | Done: per-board SQLCipher file, Argon2id key |
| 13 Easy edit/save/modify | Done: forms, external editor, undo/redo, tags, start/deadline dates |
| 14 ToDo/Doing/Done | Done |

---

## Architecture, and why it is shaped this way

```
src/
  main.rs      CLI → config → open DB → bootstrap board → event::run
  event.rs     terminal lifecycle, panic-safe restore, THE event loop owns the Terminal
  app.rs       App state, popup stack, action dispatch, board switching/unlocking
  keymap.rs    BINDINGS: single source of truth for BOTH dispatch and footer hints
  crypto.rs    Argon2id → 32-byte key, Zeroizing
  editor.rs    suspend TUI → external editor → restore
  undo.rs      invertible command stack (in memory, per session)
  domain/      Board, Task/Status/Priority/Tag, Note
  storage/     TaskStore trait, main_db (plain), locked_db (SQLCipher),
               task_store_impl (shared SQL), migrations
  ui/          board, footer (hint bar), popup, forms, theme
```

Three decisions are load-bearing; changing them will reintroduce bugs that were
already fixed once.

**1. The event loop owns the one `Terminal`; `App` never runs the editor.**
`App` only *records* an edit request (`pending_edit`), and `event.rs` performs it
with its own `Terminal`. An earlier version built a *second* `Terminal` over the
same stdout inside `App`; `clear()` on it then failed with *"the cursor position
could not be read within a normal duration"*, the `?` propagated, and **every
edit the user typed was silently discarded** while temp files leaked into `/tmp`.
Two rules in `editor.rs` follow from that and must not be relaxed: the edited
text is read into memory *before* any terminal handling, and no
terminal-restoration step may fail the call. Temp-file cleanup is a `Drop` guard
so it also runs on an unwind. Regression tests cover all of this.

**2. `keymap::BINDINGS` generates the footer hints.** The hint bar is not a
hardcoded string. Add a binding there and the footer picks it up; hardcode one in
`ui/footer.rs` and the two will drift.

**3. `ActiveStore` is an enum, not `Box<dyn TaskStore>`.** A boxed trait object
is assumed by the borrow checker to run arbitrary code on drop, which extends the
borrow of `self` past last use and breaks the `store.foo(); self.message = …`
pattern used throughout `app.rs`. The enum has no `Drop`, so NLL behaves. The UI
never branches on board kind — plain and encrypted boards go through the same
`TaskStore`.

---

## SQLCipher facts — verified empirically on this machine, do not re-derive

1. `PRAGMA key` must be the **first** statement on the connection.
2. Raw key is passed as `PRAGMA key = "x'<64 lowercase hex>'"`, which skips
   SQLCipher's own KDF — Argon2id is done in `crypto.rs` instead.
3. **A wrong key does not fail at `PRAGMA key`.** It fails on the first real
   read, so `locked_db` probes with `SELECT count(*) FROM sqlite_master` and maps
   the error to `StorageError::WrongPassphrase`.
4. **`PRAGMA cipher_log_level = NONE` is mandatory.** On a failed decrypt
   SQLCipher writes `ERROR CORE sqlcipher_page_cipher: hmac check failed…` to
   **stderr**, which corrupts the TUI. It must be set *after* `PRAGMA key`
   (the codec it configures does not exist until the connection is keyed).
5. Feature flag: `bundled-sqlcipher-vendored-openssl`. ~51s cold build, needs
   `perl`. `ldd` shows no libssl/libcrypto — this is what makes one binary work
   across Debian and Mint, whose OpenSSL versions differ.

Verified encryption properties:

```
main.db     → "SQLite format 3"        (plain, by design)
board-N.db  → random header, file(1) says "data"
sqlite3 board-N.db .tables → Error: file is not a database
grep -r SECRET_CANARY <data dir> → nothing
```

`main.db` stores only a locked board's name, path, KDF salt and cost parameters
— never task or note content, not even counts. The salt lives in the plaintext
DB deliberately: it is not secret, and SQLCipher leaves no plaintext header to
put it in.

**File permissions**: `main.db` holds *plaintext* tasks for unlocked boards, so
database files are forced to `0600` and the data directory to `0700`, applied
before the first write so SQLite's `-wal`/`-shm` sidecars inherit it. They were
previously world-readable at the mercy of the umask. Tests assert both.

---

## Environment specifics on this machine

- **tmux prefix is `C-z`**, not `C-b` — nothing in the app binds `C-z`.
- tmux has `default-terminal "screen-256color"` with a `Tc` override that only
  matches `xterm-256color*`, so **truecolor does not currently reach apps inside
  tmux**. The theme layer degrades truecolor → 256 → 16. The one-line tmux fix
  belongs in the README (not yet written).
- `$EDITOR` and `$VISUAL` are **unset**; nvim and vim are both installed.
- **Editor resolution is deliberately not environment-first**: config `editor` →
  **nvim** → `$VISUAL` → `$EDITOR` → vim. nvim must keep outranking the
  environment variables — this is a standing user requirement, and deferring to
  an unset `$EDITOR` would silently land on `vi`/`nano`. A test guards it.
- The repo lives under `~/Dropbox`, so the database default is
  `~/.local/share/tsk`, never a synced directory — concurrent sync corrupts
  SQLite WAL files.
- `~/.config/nvim` is AstroNvim (leader `Space`), which is why `<Space>b` also
  opens the board switcher.

---

## Keybindings

`h/l` column · `j/k` move · `gg`/`G` · `^d`/`^u` · `Tab` pane · `a`/`i` new ·
`e` edit · `o` body in nvim · `H`/`L` move column · `J`/`K` reorder · `m`/`p`/`t`
dropdowns · `dd` delete · `c` capture note · `N` notes pane · `gp` promote ·
`b` or `<Space>b` boards · `S` status dropdown · `u`/`^r` undo/redo ·
`/` search (`n`/`N` next/prev match, `Esc` or `:nohl` clears) · `:` command ·
`?` help · `ZZ` quit. In the notes pane `a`/`e`/`dd` act on notes. Commands: `:board new <name> [locked]`,
`:board rename <name>`, `:board delete <name>`, `:lock`, `:q`.

---

## Card colour rule (user-specified)

Card colour is driven by the **deadline**, not by status — status is already
conveyed by which column a card sits in:

| Condition | Colour |
|---|---|
| no due date | **grey** |
| more than 15 days out | green |
| 14–5 days | yellow |
| 4–2 days | orange (256-colour 208; degrades to yellow at 16 colours) |
| less than 2 days | red |
| overdue | black background, white text |

The user's stated ranges leave exactly 15 days unassigned; 15 is treated as
green (`Urgency::Distant`).

**All six colours are configurable** in `config.toml` under `[theme]` as
`deadline_none`, `deadline_distant`, `deadline_soon`, `deadline_near`,
`deadline_imminent`, `deadline_overdue_fg`, `deadline_overdue_bg`. See
`config.example.toml` for a documented template. Hex values like `#ff8700`
degrade automatically: exact RGB on truecolor, nearest 256-colour otherwise.
Cards show a right-aligned deadline badge (`3d`, `-2d` when overdue).

A bug worth remembering: the 256-colour quantiser originally assumed the
xterm colour cube was evenly spaced. It is not — the levels are
`0, 95, 135, 175, 215, 255` — so `#ff8700`, which *is* exactly index 208,
quantised to 214. `nearest_ansi256` now snaps to the real levels and also
considers the 24-step greyscale ramp.

## Open work, roughly in priority order

1. **All-locked boards cannot start.** `App::ensure_startable` refuses to open
   directly onto a locked board because no passphrase prompt exists before the
   TUI loop begins. If every board is locked, the CLI will not start. Fix by
   prompting at startup.
2. **`j`/`k` can land on a hidden row while a search filter is active.** Search
   hides non-matching rows at render time, but `col.selected` still indexes the
   full list; only `n`/`N` walk the matching subset. Either filter the backing
   list or make plain movement skip hidden rows.
3. **A status-changing edit takes two undos.** Editing a task and changing its
   status in the same form pushes two undo commands (the field update, and the
   `move_task` that keeps column positions dense). Collapse them into one.
4. **The form's own Tags field is still local-only.** Card-level tagging (`t`)
   persists correctly; the Tags row inside the task form does not write through.
5. **README + packaging**: `cargo-deb` for Debian. `install.sh` covers
   source installs already.
6. **Linux Mint MATE is unverified.** Plan is a static
   `x86_64-unknown-linux-musl` build as the universal artifact, which avoids the
   glibc skew between Debian 13 (2.41) and Mint 22 (2.39). **Blocked**: needs
   `sudo apt install musl-tools`, which was never run. Building natively on the
   Mint machine (`./install.sh`) is the alternative.
7. **The tmux floating editor is not verified against a real tmux popup.** The
   `editor_invocation` split is unit-tested both ways, but nobody has watched
   `tmux display-popup` actually run nvim over the board. The suspend/restore
   sequence is still applied in the tmux path, which may be unnecessary.
8. Undo/redo is in-memory per session and does not survive a restart. Normal for
   a vim-style tool; confirm with the user if persistence is wanted.

---

## Schema versions

`schema_version` is at **2**. Version 1 is the original tables; version 2 adds
`tasks.start_date` and `tasks.deadline` (both nullable unix seconds) to the main
and locked-board schemas. **Never edit an existing migration** — append a new
version. A real v1 database was upgraded in place and verified to keep its rows.

## Testing notes for whoever picks this up

`cargo test` is fully headless — `App` is constructed from
`MainDb::open_in_memory()` and the TUI is driven through
`ratatui::backend::TestBackend`. No test touches the real `~/.config` or
`~/.local/share`.

For real-terminal checks there is a PTY harness at
`<scratchpad>/ptydrive.py` (session-scoped; recreate it if gone). It spawns the
binary under a pty, sets the window size, sends keys and renders the frames via a
small ANSI emulator. **Two traps I hit with it**: a drain loop with a timeout
above ~250ms spins forever against the app's redraw tick and looks exactly like
an application hang; and `pkill -f 'target/debug/tsk'` also matches the harness's
own shell wrapper and kills the test. Subagent reports were repeatedly correct
about unit tests and wrong about real-terminal behaviour — the editor data-loss
bug passed 149 tests and only showed up under a real pty. Verify in a pty.
