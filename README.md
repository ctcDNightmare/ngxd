# ngxd — ngx daemon

A Paperless-ngx upload daemon for Windows, Linux, and macOS. Watch a folder, upload documents, poll their processing tasks, then delete the local files **only after confirmed success**.

Each platform gets one executable; users need neither Rust nor OpenSSL installed.

## Quick start

Build with Rust 1.92 or newer, then initialize the config:

```sh
cargo build --release --locked
./target/release/ngxd init
./target/release/ngxd config-path
```

Edit the printed `config.json`:

```json
{
  "folder": "/absolute/path/to/paperless-inbox",
  "api_key": "YOUR_PAPERLESS_API_TOKEN",
  "endpoint_url": "https://paperless.example.com",
  "tag_ids": [],
  "recursive": false
}
```

Use your Paperless base URL without `/api`, and a token from your Paperless profile with document-upload, tag, and task-read permissions. `tag_ids` is optional; omit it or use `[]`. Set `recursive` to `true` to include subfolders (default: `false`). Windows paths can use forward slashes, e.g. `C:/Users/Alice/Scans`.

```sh
./target/release/ngxd run
```

Config lives in your OS's user config directory. `--config /path/config.json` overrides it on any command. Restart after editing. Keep config outside the watched folder and restrict access: it contains your API token.

## Run as a service

Move the executable to a permanent location first, then:

```sh
ngxd install
ngxd start
ngxd status
ngxd stop
ngxd uninstall
```

Installation saves the executable and config paths; it does not start the daemon. Uninstall keeps config and queued jobs.

| OS | Service and logs |
| --- | --- |
| Linux | systemd **user** service. Run without sudo. Logs: `journalctl --user -u ngxd -f`. |
| macOS | LaunchAgent for the current user; runs at GUI login. Logs: `~/Library/Logs/ngxd.log`. |
| Windows | Native service; management requires elevation. Defaults to **LocalSystem**, whose folder/network permissions differ from yours. Protect executable/config ACLs or configure a dedicated account. Use foreground `run` for logs; Event Log integration is not implemented. |

Linux requires a running user service manager; enable lingering separately if it must run without login. Moving the executable breaks registration. Native service lifecycle testing on Windows/macOS is still outstanding.

## Recovery and file handling

- The configured folder is scanned at startup and periodically; subfolders are included only with `"recursive": true`. Symlinks, empty files, dotfiles, and `.part`, `.tmp`, and `.crdownload` files are skipped.
- Files must remain unchanged for two seconds before upload. For reliable handoff, write to a temporary file and rename when finished; a quiet interval alone cannot prove writing is complete.
- Snapshots and task UUIDs are saved in `config.json.queue/` beside the config. Keep this directory while jobs remain. Restart resumes saved tasks; only one process may use a queue.
- Network/server upload failures retry after 30 seconds; task polling repeats every three seconds. A request can take up to 120 seconds, including during shutdown.
- Processing failures and permanent upload errors retain the source and snapshot. Fix the cause, stop ngxd, then run `ngxd retry-failed` to resubmit failed snapshots. This can create another remote document.
- Cleanup checks the file against the uploaded content and preserves replacements. Interrupted cleanup may leave `.ngxd-<id>-cleanup` files; check logs before restoring these manually.

A lost upload response can cause a duplicate submission. Duplicate-document failures retain the local file; they are not treated as success. Changing the endpoint or folder while jobs remain is rejected—use a separate config for another destination.

## Building and checking

CI builds Linux x64 musl, Windows x64 MSVC, and macOS ARM64 executables. For standalone Linux builds, install a musl C toolchain and use:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl
```

Default Linux GNU builds depend on the host glibc version. Windows MSVC builds bundle the C runtime through `.cargo/config.toml`; macOS uses the OS's system frameworks.

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked
python3 tests/smoke.py  # Linux/macOS; temporary files and a tiny API stub
```

Tests never install services or contact your Paperless instance. [Paperless API reference](https://docs.paperless-ngx.com/api/#file-uploads).
