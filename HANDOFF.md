# HANDOFF — `tsk`

Terminal kanban task manager and quick note taker. Vim-native, keyboard-only,
SQLite-backed, with password-protected boards whose database file is itself
encrypted.

**Status**: phases 0–8 of the approved plan are done and verified. 178 tests
pass, `cargo clippy --all-targets -- -D warnings` is clean, ~7,400 lines.
Approved plan: `~/.claude/plans/silly-tinkering-perlis.md`.

**The repository has no commits yet** — everything is untracked working tree.
Nothing has been committed because it was never asked for.

---

## How to build and run

```sh
export PATH="$HOME/.cargo/bin:$PATH"     # rustup toolchain, rustc 1.98.1
cargo build                              # or --release
cargo test                               # 178 tests, all headless
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
| 6 Tasks coloured by status | Done, plus priority markers `!` / `!!` |
| 7 Manual status change | Done: `H`/`L`, or the `m` dropdown |
| 8 Kanban like kanban-tui | Done: three columns, counts in titles |
| 9 Configurable db/config paths | Done: CLI flag → env → config file → XDG |
| 10 Debian-like + tmux | Works on Debian 13 in tmux; **Mint not yet tested** (see Open work) |
| 11 Multiple boards | Done: switcher, `:board new/rename/delete` |
| 12 Password-secured board | Done: per-board SQLCipher file, Argon2id key |
| 13 Easy edit/save/modify | Done: forms, `$EDITOR`, undo/redo |
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
`b` or `<Space>b` boards · `u`/`^r` undo/redo · `/` search · `:` command ·
`?` help · `ZZ` quit. Commands: `:board new <name> [locked]`,
`:board rename <name>`, `:board delete <name>`, `:lock`, `:q`.

---

## Open work, roughly in priority order

1. **Search is a stub** (`src/app.rs:1402`). `/`, `n`, `N` enter the mode and
   manage the buffer, but nothing filters. `N` is already disambiguated
   (notes-pane toggle vs previous-match) in `keymap::resolve`.
2. **All-locked boards cannot start.** `App::ensure_startable` refuses to open
   directly onto a locked board because no passphrase prompt exists before the
   TUI loop begins. If every board is locked, the CLI will not start. Fix by
   prompting at startup.
3. **Tag persistence is UI-only.** The `t` dropdown and the form's Tags field are
   wired but write nothing; the `tags`/`task_tags` tables exist and are unused.
   Either finish it (add tag methods to `TaskStore`, append a v2 migration rather
   than editing v1 — users may have a v1 DB on disk) or hide the control. Tags
   are not among the 14 stated requirements; they were an addition.
4. **README + packaging**: `cargo-deb` for Debian, plus the tmux truecolor note
   and required apt build deps.
5. **Linux Mint MATE is unverified.** Plan is a static
   `x86_64-unknown-linux-musl` build as the universal artifact, which avoids the
   glibc skew between Debian 13 (2.41) and Mint 22 (2.39). **Blocked**: needs
   `sudo apt install musl-tools`, which was never run. Building natively on the
   Mint machine is the alternative.
6. Undo/redo is in-memory per session and does not survive a restart. Normal for
   a vim-style tool; confirm with the user if persistence is wanted.

---

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
