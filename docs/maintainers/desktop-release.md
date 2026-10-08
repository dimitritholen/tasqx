# Building and signing Tasqx Desktop

Tasqx Desktop (`apps/tasqx-desktop`) ships **unsigned** for its first release
(D???). CI builds the unsigned bundles for macOS, Windows and Linux; signing is a
step a maintainer adds later with their own identities. This page covers both:
building a bundle yourself from a clean clone, and the signing inputs the build
reads when you have them.

Nothing here tags, pushes or publishes. `.github/workflows/desktop.yml` runs
with a read-only token and only uploads workflow artifacts; attaching a desktop
bundle to a GitHub release is a decision for later and is not wired anywhere.

## Getting the CI bundles

Every run of the **Desktop** workflow uploads one artifact per runner:
`tasqx-desktop-macos-latest`, `tasqx-desktop-windows-latest` and
`tasqx-desktop-ubuntu-latest`. It runs on pull requests that touch
`apps/tasqx-desktop`, `scripts/desktop-bundle.sh` or the workflow, on pushes to
`main` that touch them, on every `v*` tag, and on demand:

```console
gh workflow run desktop.yml --ref main
gh run download <run-id> --dir desktop-bundles
```

None of its jobs is a required check on `main`.

## Building locally

Every platform needs Node 22 with npm and stable Rust (`rustup`). Then, from the
repository root:

```console
cd apps/tasqx-desktop
npm ci
cd ../..
scripts/desktop-bundle.sh
```

`scripts/desktop-bundle.sh` is the same command CI runs. With no signing
variables set it builds unsigned and prints one `off` line per signing input.
Arguments after it go to `tauri build`, for example `--bundles dmg`. The
bundles land under `apps/tasqx-desktop/src-tauri/target/release/bundle/`.

### macOS

Prerequisite: the Xcode command line tools (`xcode-select --install`).

Output: `macos/Tasqx Desktop.app` and `dmg/Tasqx Desktop_<version>_<arch>.dmg`.

An unsigned bundle downloaded through a browser carries the quarantine
attribute, and Gatekeeper refuses it (often as "damaged"). To run your own
build, or a CI artifact you trust:

```console
xattr -dr com.apple.quarantine "/Applications/Tasqx Desktop.app"
```

### Windows

Prerequisites: the Visual Studio C++ build tools (the "Desktop development with
C++" workload) and WebView2, which Windows 10 and 11 already have. Run the script
from Git Bash.

Output: an `.msi` under `msi/` and a `-setup.exe` under `nsis/`.

SmartScreen stops an unsigned installer: choose **More info**, then **Run
anyway**.

### Linux

Prerequisites (Debian/Ubuntu names, the same list CI installs):

```console
sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Output: `deb/*.deb`, `rpm/*.rpm` and `appimage/*.AppImage`.

## Signing

The build signs each platform for which it finds an identity, and leaves the
rest unsigned. The inputs are environment variables locally and repository
secrets in CI; on a `v*` tag `desktop.yml` passes the secrets of the same names
through, and off a tag it passes none. A tag run with no secrets set still
passes and uploads unsigned bundles.

| Variable / secret | What it holds |
|---|---|
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <name> (<team id>)` |
| `APPLE_CERTIFICATE` | the Developer ID certificate and key as a `.p12`, base64-encoded (CI only; locally the identity is in your keychain) |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | notarization: your Apple ID, an app-specific password for it, your team id |
| `WINDOWS_CERTIFICATE` | the Authenticode certificate as a `.pfx`, base64-encoded (CI secret) |
| `WINDOWS_CERTIFICATE_PASSWORD` | the `.pfx` password (CI secret) |
| `WINDOWS_CERTIFICATE_THUMBPRINT` | locally: the thumbprint of that certificate in your user store (CI derives it) |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the Tauri updater keypair's private half and its password |

### macOS: sign and notarize

1. In the Apple Developer account, create a **Developer ID Application**
   certificate and install it in your login keychain.
2. Find the identity string: `security find-identity -v -p codesigning`.
3. Create an app-specific password at appleid.apple.com.
4. Build:

   ```console
   APPLE_SIGNING_IDENTITY="Developer ID Application: Jane Doe (ABCDE12345)" \
   APPLE_ID=jane@example.com APPLE_PASSWORD=<app-specific password> \
   APPLE_TEAM_ID=ABCDE12345 \
   scripts/desktop-bundle.sh
   ```

   Tauri signs the app, submits it to Apple's notary service, waits and staples
   the ticket.
5. Check it: `codesign --verify --deep --strict --verbose=2 "<bundle>/macos/Tasqx
   Desktop.app"` and `spctl -a -vv "<…>/Tasqx Desktop.app"`, which should say
   `source=Notarized Developer ID`.

For CI, export the certificate from Keychain Access as a `.p12`, then store
`base64 -i cert.p12` as `APPLE_CERTIFICATE` and its password as
`APPLE_CERTIFICATE_PASSWORD`, beside the four values above. Tauri imports the
`.p12` into a throwaway keychain itself.

### Windows: Authenticode

1. Import your `.pfx` into the current user's certificate store:
   `Import-PfxCertificate -FilePath cert.pfx -CertStoreLocation Cert:\CurrentUser\My -Password (Read-Host -AsSecureString)`
   prints its thumbprint.
2. Build from Git Bash:
   `WINDOWS_CERTIFICATE_THUMBPRINT=<thumbprint> scripts/desktop-bundle.sh`.
   It signs with SHA-256 and timestamps through DigiCert's server; set
   `WINDOWS_TIMESTAMP_URL` to use your CA's.
3. Check it: `Get-AuthenticodeSignature "<…>\msi\*.msi"` should say `Valid`.

For CI, store `[Convert]::ToBase64String([IO.File]::ReadAllBytes("cert.pfx"))`
as `WINDOWS_CERTIFICATE` and the password as `WINDOWS_CERTIFICATE_PASSWORD`. The
workflow imports it on the Windows runner and passes the thumbprint on.

### Updater keypair

Generate the pair once, outside the repository, and keep the private key and its
password somewhere you will not lose them; an app built against a public key
accepts updates signed only by its private half:

```console
cd apps/tasqx-desktop
npx tauri signer generate -w ~/.tauri/tasqx-desktop.key
```

The private key (the file's contents) and its password become
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The public
key belongs in `src-tauri/tauri.conf.json` under `plugins > updater`, together
with the updater plugin itself, which the app does not carry yet. Until it does,
`tauri build` cannot produce updater artifacts, so the script reports a set key
and ignores it; once `plugins > updater` is in the config it writes a `.sig`
beside every updater bundle with no change to the script or the workflow.

## Rolling back

The desktop app keeps no data of its own: tasks, memory and links live in the
tasqx store the daemon serves (D160). Going back a version is installing the
previous bundle over the current one, from that run's artifacts. Before trying a
new build on a store you care about, `tasqx export > backup.json` gives you a
copy, links included (D181), that `tasqx import backup.json` restores into a
fresh store.
