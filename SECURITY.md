# Security Policy

## Reporting a Vulnerability

**Do not open a public issue for security problems.**

Report privately via GitHub's [private vulnerability reporting](https://github.com/juninmd/tokenix/security/advisories/new)
(Security → Advisories → "Report a vulnerability"). Include a description, affected
version, and reproduction steps. Expect an acknowledgement within 7 days.

## Supported Versions

Only the latest released version receives security fixes. tokenix releases roll
forward; upgrade to the newest tag rather than expecting backports.

## Supply-Chain Hardening

This project defends its build and release pipeline against supply-chain attacks:

- **Pinned actions** — every GitHub Action is pinned to a full commit SHA, never a
  mutable tag, so a compromised or force-pushed upstream tag cannot inject code.
- **Least privilege** — workflows default to `permissions: contents: read`; write
  scopes are granted only to the jobs that need them.
- **Dependency policy** — `cargo-deny` (see `deny.toml`) blocks crates with known
  RUSTSEC advisories, disallowed licenses, or any source other than crates.io, on
  every PR and weekly. Dependabot keeps Cargo and Actions up to date.
- **Static workflow analysis** — `zizmor` scans every workflow for injection and
  privilege issues.
- **Egress monitoring** — `step-security/harden-runner` records network egress on CI
  runners to surface unexpected exfiltration.
- **Signed provenance** — release binaries carry SLSA build provenance
  attestations (`actions/attest-build-provenance`).
- **Tokenless publish** — crates.io publishing uses OIDC Trusted Publishing; no
  long-lived registry token is stored in the repo.
- **OpenSSF Scorecard** — the repo's posture is graded continuously.

## Verifying a Release

Each GitHub Release ships the binaries plus `sha256sums.txt`.

1. **Checksum** — confirm the download matches the published hash:

   ```sh
   sha256sum -c sha256sums.txt --ignore-missing
   ```

2. **Provenance** — verify the binary was built by this repo's Actions pipeline
   (requires the [GitHub CLI](https://cli.github.com/)):

   ```sh
   gh attestation verify tokenix-linux-x86_64 --repo juninmd/tokenix
   ```

   A successful verification proves the artifact was produced by the tokenix
   release workflow on GitHub-hosted runners and was not tampered with.
