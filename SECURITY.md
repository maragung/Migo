# Security policy

## Reporting a vulnerability

Email **security@migo.example** with a description, reproduction steps and impact.
Please do not open a public issue for anything exploitable.

- Acknowledgement within **2 business days**.
- Triage and severity assessment within **5 business days**.
- Fix timeline shared with you, and credit in the release notes if you want it.

## Scope

In scope: the server (`server/`), web client (`clients/web`), Android client, shared
packages, protocol design, and the infrastructure definitions in `infra/`.

Especially interesting to us: anything that reaches private message plaintext, any
authentication or authorization bypass, key handling flaws, replay or IDOR issues,
remote resource exhaustion in the decoder or fanout path, and rate-limit bypasses.

Out of scope: findings that require a compromised device OS or platform keystore, social
engineering, volumetric DDoS demonstrations, and reports produced solely by an automated
scanner with no demonstrated impact.

## Our commitments

- Audited cryptographic libraries only; no in-house primitives.
- No plaintext of private messages on any server, for any role, including administrators.
- No secrets in source control; production secrets come from a secret manager.
- Security fixes ship ahead of features, and dependency audits run on every build.

See [docs/03-security-threat-model.md](docs/03-security-threat-model.md) for the full
threat model.
