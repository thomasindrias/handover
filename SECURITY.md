# Security policy

## Supported versions

Sesh is pre-release software. Security fixes are made on the current `main`
branch and will be included in the next release. Older revisions are not
separately supported.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
private vulnerability reporting for this repository so the report and any
supporting material remain private.

Include a clear description, affected version or commit, reproduction steps,
impact, and any suggested mitigation. Remove credentials and unrelated personal
data before attaching logs or session files. You can expect an acknowledgement
within three business days and a status update within seven business days.

If private vulnerability reporting is temporarily unavailable, contact the
repository owner through [their GitHub profile](https://github.com/thomasindrias)
without publishing exploit details.

## Trust model

Sesh stores session data as plaintext on the local machine. It applies private
Unix permissions (`0700` directories and `0600` files), rejects unsafe canonical
state paths and permissions, and keeps its state outside application repositories.
It does not encrypt or automatically redact stored content.

An unrestricted coding provider launched by Sesh runs as your Unix user. Sesh is
not a sandbox or a security boundary between processes owned by that user. Review
provider permissions, hooks, and configuration before trusting them, and protect
machine access and backups accordingly.

Security-sensitive behavior and limitations are documented in
[docs/architecture.md](docs/architecture.md) and
[docs/providers.md](docs/providers.md).
