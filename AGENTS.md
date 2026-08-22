## Learned User Preferences

## Learned Workspace Facts

- biTurbo is a Tauri desktop app (macOS / Windows / Linux).
- Installed app locations:
  - macOS: `/Applications/biTurbo.app`
  - Windows: `%LOCALAPPDATA%\biTurbo\biTurbo.exe`
  - Linux: `/usr/bin/biturbo` (or wherever the package installs it)
- The MCP server binary is bundled inside the app (Tauri `externalBin`):
  - macOS: `/Applications/biTurbo.app/Contents/MacOS/biturbo-mcp`
  - Windows: `%LOCALAPPDATA%\biTurbo\biturbo-mcp.exe`
  - Linux: `biturbo-mcp` (on `$PATH` after package install)
- Cursor MCP config belongs in `~/.cursor/mcp.json` or project `.cursor/mcp.json`; `command` must be an absolute path to `biturbo-mcp`.
- GUI and MCP share the same data directory:
  - macOS: `~/Library/Application Support/com.biturbo.app`
  - Windows: `%APPDATA%\com.biturbo.app`
  - Linux: `~/.local/share/com.biturbo.app`
- Bundle identifier is `com.biturbo.app`.
- Dev MCP binary paths: `src-tauri/target/debug/biturbo-mcp` or `src-tauri/target/release/biturbo-mcp`.
- Local signed build (no notarization): `pnpm tauri:build`.
- Unsigned build: `pnpm tauri build -- --no-sign`.
- macOS notarized release build: `pnpm tauri:build:notarized` with `APPLE_ID`, `APPLE_PASSWORD` (app-specific password), and `APPLE_TEAM_ID` exported.
