# Signed app updates

see.computer checks GitHub Releases for signed updates after launch and once each day. When it finds a newer version, it downloads the macOS updater archive in the background. Tauri verifies the archive signature before the app saves it to a private temporary file.

The app does not install or restart during a background check. The tray action changes to `Restart to update <version>` after the download finishes. Installation starts only when the user chooses that action. The session controller permits it only while idle, then the app installs the cached archive and restarts.

The cache uses an unlinked file so a ready update does not occupy RAM while you dictate. It lasts for the current process. Quitting before installation discards the download.

## Bootstrap release

Version 0.1.0 and any other build without the updater cannot discover or install an update. Users of those builds must download and install the first updater-enabled release by hand. Releases after that can use the in-app update path.

## Two signing systems

macOS distribution and Tauri updates use different keys.

Apple Developer ID signing identifies the app to macOS. Apple notarization lets Gatekeeper verify that Apple scanned the distributed build. These steps protect the `.app` and `.dmg` that a user opens.

Tauri updater signing uses a separate Minisign key pair. The app contains the public key and uses it to verify each downloaded `.app.tar.gz`. The release job uses the private key to create the matching `.sig` file. Tauri does not allow signature verification to be disabled.

Both systems must be ready before publishing an updater-enabled release.

## Prerequisites

- An active Apple Developer account with a Developer ID Application certificate.
- Apple notarization credentials for the same team.
- A Tauri updater signing key pair stored outside the repository.
- The updater public key in `src-tauri/tauri.conf.json`.
- The updater private key and its password stored only as GitHub Actions secrets.

The updater public key is embedded in `src-tauri/tauri.conf.json`. The private key is kept outside the repository and stored as a GitHub Actions secret. Never commit it. Losing it requires a manual app update to trust a replacement key.

The release workflow requires these GitHub Actions secrets:

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_PASSWORD`
- `APPLE_TEAM_ID`
- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, only when the key has a password

The workflow stops before the build if any required secret is missing. The installed updater key has no password and is protected by local file permissions and GitHub secret storage. It does not print secret values.

## Release review

Push a version tag only after the Cargo package version and Tauri configuration version match the tag. The workflow builds Apple Silicon `.app` and `.dmg` bundles. Tauri also creates the `.app.tar.gz` updater archive and its `.sig` file. `tauri-action` writes `latest.json` with a `darwin-aarch64` entry, the archive URL, and the literal signature text.

The workflow creates a draft release and never publishes it. Before publishing, confirm that the draft contains the DMG, updater archive, signature, and `latest.json`. Check that the manifest version matches the tag and that its updater URL names the uploaded `.app.tar.gz` file.

The configured endpoint uses GitHub's `/releases/latest/download/latest.json` route. Drafts and prereleases do not appear at that route. The endpoint starts serving the manifest only after someone publishes the draft as a regular release.
