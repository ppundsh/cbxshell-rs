# release.config.psd1 — the only project-specific values of the shared release standard
# (see the maintainer's release standard). Scripts that read it:
#   release-manifest.ps1, scripts\approve-release.ps1
@{
    # GitHub owner/repo that hosts the releases
    Repo              = 'ppundsh/cbxshell-rs'
    # Installer built into dist\ and attached to every release
    Asset             = 'CBXShell-Setup.exe'
    # First line of the approval statement; must equal the app updater's APPROVAL_NAMESPACE
    ApprovalNamespace = 'cbxshell-release'
    # Where the version lives, and a regex whose first group captures it
    VersionFile       = 'Cargo.toml'
    VersionRegex      = '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"'
}
