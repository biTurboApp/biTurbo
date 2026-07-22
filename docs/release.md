# Releasing biTurbo

biTurbo is built and released through GitHub Actions. The release pipeline produces installers for Windows, Linux, macOS Intel, and macOS ARM, and bundles the `biturbo-mcp` MCP server binary inside each app.

## Quick start

1. Make sure all changes are committed and pushed to `main`.
2. Bump the version in `package.json`, both lockfiles, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, and `src-tauri/tauri.conf.json`.
3. Run the release script:

   ```bash
   npm run release
   ```

This reads the current version from `package.json`, creates a git tag like `v0.3.0`, and pushes it to GitHub. The tag triggers the [`.github/workflows/release.yml`](../.github/workflows/release.yml) workflow, which builds the standard platform targets and creates a draft GitHub Release. CUDA MCP artifacts are produced separately by `cuda-runtime.yml` on the NVIDIA runner.

## Versioning

The release script uses the version in `package.json`. Keep all release metadata in sync:

- `package.json` → `"version"`
- `package-lock.json` → root package versions
- `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock` → crate version
- `src-tauri/tauri.conf.json` → `"version"`

If the tag already exists (e.g. `v0.1.0`), the script refuses to run. Bump the version, commit, and push before releasing again.

## What the pipeline builds

| Target | Runner | Output |
|--------|--------|--------|
| Windows x64 | `windows-latest` | `.msi` installer |
| macOS ARM | `macos-latest` | `.dmg` for Apple Silicon |
| macOS Intel | `macos-latest` | `.dmg` for Intel Macs |
| Linux x64 CPU | `ubuntu-latest` | `.deb` and AppImage |
| Linux x64 CUDA MCP | self-hosted NVIDIA | separate `biturbo-mcp` archive |

The pipeline also bundles the `biturbo-mcp` MCP server binary as a Tauri `externalBin` sidecar. It is built separately for each target and placed at `src-tauri/binaries/biturbo-mcp-<target-triple>` before the app is packaged.

## macOS code signing

The pipeline signs macOS builds when the required Apple Developer secrets are present in GitHub. If the certificate secret is missing, macOS builds fall back to `--no-sign` (unsigned) so the pipeline still runs.

Required secrets for signing:

- `APPLE_CERTIFICATE` — base64-encoded `.p12` Developer ID certificate
- `APPLE_CERTIFICATE_PASSWORD` — password for the `.p12` file
- `KEYCHAIN_PASSWORD` — temporary keychain password (any secure value)
- `APPLE_ID` — Apple ID for notarization
- `APPLE_PASSWORD` — app-specific password for the Apple ID
- `APPLE_TEAM_ID` — Apple Developer Team ID

Add these in **Settings → Secrets and variables → Actions** in the GitHub repository.

## Manual release trigger

You can also run the workflow manually from the GitHub Actions tab:

1. Go to **Actions → Release → Run workflow**.
2. Select the branch and click **Run workflow**.

This is useful for testing the pipeline without creating a new tag.

## Local builds

To build the app locally without creating a release:

```bash
# Standard local release build (unsigned, host platform only)
npm run tauri:build

# macOS notarized release build (requires signing env vars)
APPLE_ID=... APPLE_PASSWORD=... APPLE_TEAM_ID=... npm run tauri:build:notarized
```

These scripts build the `biturbo-mcp` sidecar first and then run `tauri build`.

## Development commands

```bash
# Run the Tauri app in dev mode
npm run tauri:dev

# Run the MCP server standalone
npm run mcp:dev

# Build the MCP server standalone
npm run mcp:build

# Run MCP smoke tests
npm run mcp:test
```

## Troubleshooting

### Release workflow fails at the `externalBin` step

The sidecar binary must exist at `src-tauri/binaries/biturbo-mcp-<target-triple>` before Tauri bundles the app. The CI and local scripts create this automatically. If you run `cargo build` or `tauri build` directly without the helper scripts, create the placeholder manually:

```bash
node scripts/ensure-sidecar-placeholder.mjs
```

### Tag already exists

The release script refuses to create a tag if it already exists locally or on the remote. Bump the version in `package.json` and `src-tauri/tauri.conf.json` first.

### macOS signing is skipped

Check that all required Apple secrets are set in GitHub Actions. If any are missing, the macOS builds will run with `--no-sign` and produce unsigned `.dmg` files.

## Files involved

- `package.json` — version, `release` script, `tauri:build` scripts
- `src-tauri/tauri.conf.json` — app version, `externalBin` sidecar config
- `.github/workflows/release.yml` — CI pipeline
- `scripts/release.mjs` — creates and pushes the version tag
- `scripts/build-sidecar.mjs` — builds the sidecar for release/local builds
- `scripts/ensure-sidecar-placeholder.mjs` — creates a placeholder for dev builds
