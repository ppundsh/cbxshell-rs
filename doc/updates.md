# Installation, Version Check & Auto-Update

CBX Shell ships as a single installer, `CBXShell-Setup.exe`, installed per user without administrator rights. The manager checks GitHub Releases for a newer version and, once the user agrees, downloads it, **verifies its digital signature** and upgrades in place. The design follows the Tauri updater used by WinNUT-Client-RS (the same minisign / Ed25519 signatures and `latest.json` manifest) without depending on Tauri.

---

## 1. Installer

| Item | Details |
|---|---|
| Location | `%LOCALAPPDATA%\Programs\CBXShell` (override with `--dir`) |
| Privileges | `asInvoker`; writes HKCU only; no UAC prompt |
| Uninstall | Registered under *Settings › Apps › Installed apps* (`DisplayVersion`, `UninstallString`, `QuietUninstallString`, `ModifyPath`) |
| Start menu | "CBX Shell" shortcut to `cbx-manager.exe` |

Command line:

| Argument | Effect |
|---|---|
| (none) | Graphical setup wizard |
| `--quiet` / `--silent` / `/s` | Silent install with default options |
| `--dir <folder>` | Install folder |
| `--uninstall [--quiet]` | Uninstall |
| `--update --dir <folder> [--wait-pid <PID>]` | **In-place update**: replaces program files, re-registers `cbxshell.dll` and updates `DisplayVersion`; **keeps every setting, file association and shortcut**; restarts the manager afterwards. Used by the manager's updater |
| `--lang zh-TW|en` | Installer language |

Files locked by Explorer (such as a loaded `cbxshell.dll`) are renamed to `*.old` before the new file is written; leftovers are removed by the next update.

---

## 2. Version Check

- The **Software update** card on the manager's *About* page shows the current version, the last check date and the status, with *Check now*, *Install* and *Releases* buttons.
- **Auto check** (on by default, registry value `AutoCheckUpdates`): when the manager starts and the last successful check (`LastUpdateCheck`, Unix seconds) is more than 24 hours old, it checks once in the background. It only reports that a new version exists — **nothing is downloaded or installed automatically**.
- Versions compare as `major.minor.patch`; with equal numbers a release beats a pre-release (`2.0.1` > `2.0.1-beta.1`); unparseable versions are never treated as newer.

Manifest URL (always the latest stable release):

```
https://github.com/ppundsh/cbxshell-rs/releases/latest/download/latest.json
```

`latest.json` uses the Tauri shape:

```json
{
  "version": "2.0.2",
  "notes": "Release notes",
  "pub_date": "2026-11-20T00:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "<contents of CBXShell-Setup.exe.sig>",
      "url": "https://github.com/ppundsh/cbxshell-rs/releases/download/v2.0.2/CBXShell-Setup.exe"
    }
  }
}
```

---

## 3. Update Flow

1. The user clicks *Install* and confirms the dialog.
2. The installer is downloaded with WinHTTP (system proxy settings, TLS validated by Windows, GitHub's redirect to its CDN followed) with a progress display.
3. **Signature check**: the minisign (Ed25519) signature is verified against the public key compiled into the manager (`assets/updater.pub`). On failure the update stops; nothing is written or executed. When the build lists YubiKey approver keys, the maintainer's approval must verify as well (section 4).
4. The file is written to `%TEMP%\CBXShell-Update\CBXShell-Setup-<version>.exe` and started with `--update --dir <install folder> --wait-pid <manager PID>`; the manager then closes.
5. The installer waits for the manager to exit, replaces the files, re-registers the extension and restarts the manager. On failure it shows the error and restarts the old manager.

### Security design

| Threat | Mitigation |
|---|---|
| Tampered installer / man-in-the-middle | The download must pass Ed25519 verification against the built-in public key; TLS is only a second layer |
| Compromised GitHub account, CI or signing key | A maintainer approval of version + SHA-256 signed with the YubiKey (FIDO2, PIN + touch) is also required; that key cannot be exported |
| Malicious manifest pointing elsewhere | `url` must start with `https://github.com/ppundsh/cbxshell-rs/releases/download/` and may not contain `..`, `?`, `#`, `@`, backslashes or spaces |
| Downgrade | Only strictly newer versions are offered |
| Resource exhaustion | Manifest ≤ 64 KiB, installer ≤ 256 MiB, JSON nesting ≤ 16 |
| Builds without a key | With no key in `assets/updater.pub` the updater is disabled and points to GitHub |
| Installing without consent | Automatic checks only notify; download and install need a click and a confirmation |

---

## 4. YubiKey Release Approval (second signature)

With only the CI-held minisign key, a compromise of the GitHub account, the Actions workflow or its secrets would let an attacker ship a correctly signed malicious update. The updater therefore also requires an **approval signed with the maintainer's YubiKey**:

| Layer | Held by | Proves |
|---|---|---|
| minisign signature (`.sig`) | GitHub Actions secret | The installer was built by CI |
| YubiKey approval (`approval` in `latest.json`) | Maintainer's YubiKey (FIDO2, key cannot be exported) | The maintainer personally approved **this version + this installer SHA-256**, with PIN and touch |

- The approved text has a fixed shape — `cbxshell-release\nversion=<version>\nsha256=<installer SHA-256>\n` — signed with `ssh-keygen -Y sign -n cbxshell-release` (SSHSIG).
- The updater rebuilds that text from the SHA-256 of what it downloaded, verifies it against the `sk-ssh-ed25519` keys in `assets/release-approvers.pub`, and requires both FIDO flags **UP (touch) and UV (PIN)**. Only hardware keys (`sk-ssh-ed25519@openssh.com`) are accepted; ordinary software SSH keys are rejected.
- As soon as `release-approvers.pub` lists a key, builds from that source **always** require an approval; a missing or invalid approval aborts the update.
- Tags must be signed with the same YubiKey (`git tag -s`); the `pre-push` hook rejects unsigned commits and tags, and CI verifies the tag signature before building.

### Either kind of YubiKey key works

| | GnuPG (OpenPGP card) | FIDO2 SSH key |
|---|---|---|
| For | An Ed25519 key moved onto the YubiKey with `gpg --edit-key` › `keytocard` | Created with `ssh-keygen -t ed25519-sk` |
| Setup script | `scripts\setup-gpg-signing.ps1` | `scripts\setup-yubikey.ps1` |
| Approval signature | `gpg --detach-sign --armor` (SHA-256 / SHA-512) | `ssh-keygen -Y sign -n cbxshell-release` |
| `release-approvers.pub` | `openpgp ssh-ed25519 AAAA… openpgp:0x…` (output of `gpg --export-ssh-key <KEYID>!`: public key value only, no e-mail) | `sk-ssh-ed25519@openssh.com AAAA…` |
| Proof of touch | Not contained in the signature; make the YubiKey require a touch for every signature with `ykman openpgp keys set-touch sig on` | UP / UV flags inside the signature, checked by the updater |

The updater picks the verifier from the `approval` format (`BEGIN PGP SIGNATURE` or `BEGIN SSH SIGNATURE`); both accept Ed25519 only, and only keys listed in `release-approvers.pub`.

---

## 5. Publisher Setup (once)

1. **YubiKey**:
   - GnuPG key already on the YubiKey (OpenPGP): run `scripts\setup-gpg-signing.ps1`. It creates or changes no keys; it finds the signing-capable Ed25519 key on the YubiKey (or the one given with `-KeyId`), sets `gpg.format=openpgp`, `gpg.program`, `user.signingkey`, always-sign for commits / tags and the `pre-push` hook, adds the public key value to `assets/release-approvers.pub`, and makes a test signature.
   - FIDO2 SSH key: run `scripts\setup-yubikey.ps1` (if the YubiKey's `ssh:github` slot is taken but this PC has no key file for it, add `-Application ssh:cbxshell -KeyName id_ed25519_sk_cbxshell` to use a new slot).

   Commit `assets/release-approvers.pub`. git's `user.email` must be one of the key's user IDs and verified on GitHub for GitHub to show "Verified".
2. **minisign key** (for CI):

   ```powershell
   npx @tauri-apps/cli@2 signer generate -w "$HOME\.tauri\cbxshell.key"
   ```

   Paste `cbxshell.key.pub` into `assets/updater.pub` and commit. **Never commit the private key.**
3. **GitHub secrets** (preferably in an Environment named `release`, see below):
   - `CBX_UPDATER_KEY`: the **contents** of the private key file
   - `CBX_UPDATER_KEY_PASSWORD`: its password (an empty string if none)
4. **Repository settings** (recommended):
   - **Settings › Environments › New environment `release`**: move both secrets there, restrict *Deployment tags* to `v*` and enable **Required reviewers** (yourself), so every release needs a click on the website before the key is released to the job.
   - **Settings › Rules › Rulesets**: for `refs/tags/v*` allow only admins to create / delete / update tags; on the default branch enable **Require signed commits** and block force pushes.
   - Account **Settings › Password and authentication**: register the YubiKey as a security key / passkey and remove weaker 2FA methods such as SMS.

> Do not swap a public key (minisign or YubiKey) once a release has shipped with it. If you must, first publish a transitional release that lists both the old and new keys and is still signed and approved with the old ones. Enrol a backup YubiKey the same way and list it in `release-approvers.pub` too, so losing the only key does not end your ability to ship updates.

---

## 6. Release Steps

1. `bump-version.ps1` → signed commit → `git tag -s vX.Y.Z -m "vX.Y.Z"` → push (each needs YubiKey PIN + touch).
2. GitHub Actions: verify the tag signature → tests → `build.ps1` → `release-manifest.ps1` (minisign + `latest.json`) → create a **draft** release. Drafts are not served by `releases/latest`, so users do not see it.
3. Locally run `scripts\approve-release.ps1 -Tag vX.Y.Z`: it checks the tag signature, downloads the draft, shows the installer SHA-256 for confirmation, signs the approval with the YubiKey, writes it into `latest.json`, uploads it and publishes the release.

Changes needed in `release.yml` (the file is protected; apply them by hand):

```yaml
jobs:
  release:
    runs-on: windows-latest
    environment: release          # secrets live only in this environment and need your approval
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Verify tag is signed by a release approver (YubiKey)
        shell: pwsh
        run: |
          git fetch --force origin "refs/tags/${{ github.ref_name }}:refs/tags/${{ github.ref_name }}"
          $keys = Get-Content assets/release-approvers.pub |
            Where-Object { $_ -match '^sk-ssh-ed25519@openssh\.com ' } |
            ForEach-Object { 'cbxshell-approver namespaces="git" ' + (($_ -split ' ')[0..1] -join ' ') }
          if (-not $keys) { throw 'assets/release-approvers.pub has no approver key' }
          $keys | Set-Content -Encoding ascii "$env:RUNNER_TEMP/allowed_signers"
          git -c gpg.format=ssh -c gpg.ssh.allowedSignersFile="$env:RUNNER_TEMP/allowed_signers" verify-tag "${{ github.ref_name }}"
          if ($LASTEXITCODE -ne 0) { throw 'tag is not signed by a release approver' }

      # With a GnuPG (OpenPGP) key use this step instead of the one above:
      - name: Verify tag is signed by a release approver (YubiKey OpenPGP)
        shell: pwsh
        run: |
          $tag = "${{ github.ref_name }}"
          $gpg = 'C:\Program Files\Git\usr\bin\gpg.exe'
          git fetch --force origin "refs/tags/${tag}:refs/tags/$tag"
          Invoke-WebRequest "https://github.com/${{ github.repository_owner }}.gpg" -OutFile "$env:RUNNER_TEMP/owner.gpg"
          & $gpg --batch --import "$env:RUNNER_TEMP/owner.gpg"
          $raw = git -c gpg.program="$gpg" verify-tag --raw $tag 2>&1 | Out-String
          if ($LASTEXITCODE -ne 0) { throw "tag signature invalid`n$raw" }
          $fpr = ([regex]'VALIDSIG ([0-9A-F]{40})').Match($raw).Groups[1].Value
          $ssh = (& $gpg --export-ssh-key "$fpr!") -split '\s+'
          $line = "openpgp $($ssh[0]) $($ssh[1])"
          if (-not (Select-String -LiteralPath assets/release-approvers.pub -SimpleMatch $line -Quiet)) {
            throw "tag signed by $fpr, which is not a release approver"
          }

      # … existing test and "Build & package installer" steps …

      - name: Sign installer & write latest.json
        shell: pwsh
        env:
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.CBX_UPDATER_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.CBX_UPDATER_KEY_PASSWORD }}
        run: .\release-manifest.ps1 -Tag "${{ github.ref_name }}"

      - name: Create draft release (published after YubiKey approval)
        uses: softprops/action-gh-release@v2
        with:
          draft: true
          files: |
            dist/CBXShell-Setup.exe
            dist/CBXShell-Setup.exe.sig
            dist/latest.json
            dist/CBXShell-Source.zip
          generate_release_notes: true
```

> `releases/latest/download/latest.json` only ever resolves to the **latest published stable release**; drafts and pre-releases are never offered to users.
