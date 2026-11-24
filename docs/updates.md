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

## Releases

A merge to `main` that changes `src-tauri/` or `ui/` starts the Release workflow. The workflow tests, builds, signs, notarizes and publishes without a manual step. Changes to the website, the iPhone app or the docs do not start it.

The workflow chooses the version. It takes the version in `src-tauri/tauri.conf.json`, or the last published release plus one patch when that version is already out. It writes the chosen version into the build, so a pull request does not need to bump it. To move to a new minor or major version, change the version in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock` in a pull request.

The workflow builds Apple Silicon `.app` and `.dmg` bundles in a draft release. Tauri also creates the `.app.tar.gz` updater archive and its `.sig` file. `tauri-action` writes `latest.json` with a `darwin-aarch64` entry, the archive URL, and the literal signature text. The workflow submits the signed DMG to Apple and staples the accepted notarization ticket to the disk image. Gatekeeper assesses both the disk image and the app inside it. The workflow also checks the Applications link and app version. It uploads the same DMG as `see.computer.dmg` for the website.

Before publishing, the workflow checks that the draft contains the DMG, updater archive, signature, and `latest.json`. It checks that the manifest version matches the release and that its updater URL points at the uploaded `.app.tar.gz` file. It then publishes the draft, tags the commit it built, and confirms that the update endpoint serves the new version. A failed run publishes nothing. Run it again from the Actions tab once the cause is fixed.

To release `main` without an app change, run the Release workflow by hand and tick `release`. Without the tick, it builds an unsigned installer preview and publishes nothing.

The configured endpoint uses GitHub's `/releases/latest/download/latest.json` route. Drafts and prereleases do not appear at that route. The endpoint starts serving the manifest only after the workflow publishes the draft as a regular release.
