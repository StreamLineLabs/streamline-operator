# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.4.x   | :white_check_mark: |
| < 0.4   | :x:                |

## Reporting a Vulnerability

Please report security vulnerabilities to **security@streamlinelabs.dev**.

**Do NOT open public issues for security vulnerabilities.**

### What to Include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

### Response Timeline

- **Acknowledgment**: Within 48 hours
- **Initial Assessment**: Within 5 business days
- **Fix Timeline**: Communicated after assessment

We follow responsible disclosure practices and will credit reporters (with permission) in our release notes.

## Dependency audit status

Release workflows run `cargo audit` and `cargo deny check advisories` in
fail-closed mode. The current lockfile has no known vulnerability advisories.
RustSec still reports unmaintained transitive crates from the Kubernetes client
stack (`backoff`, `derivative`, `instant`, and `rustls-pemfile`); these remain
dependency-health follow-up work and must not be represented as fixed.

## Security Best Practices

For production deployments, please review the [Streamline Security Documentation](https://github.com/streamlinelabs/streamline-docs).
