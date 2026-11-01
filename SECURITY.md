# Security policy

## Supported versions

Only the most recent Rustfus release is supported with security fixes.

## Reporting a vulnerability

Do not open a public issue for a vulnerability that could cause unintended disk writes, privilege-boundary bypass, or arbitrary code execution. Use the repository host's private security-advisory feature and include reproduction steps, affected version, and relevant logs with secrets removed.

Rustfus has no telemetry or update service. Users must verify the SHA-256 sidecar distributed with each release before running the application.
