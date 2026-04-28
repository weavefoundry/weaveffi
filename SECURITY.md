# Security Policy

## Reporting a Vulnerability

Please do not report security vulnerabilities through public GitHub issues,
pull requests, discussions, or social media.

Email security reports to:

<!-- TODO: replace with real email -->
security@example.com

Include as much detail as you can safely share:

- The affected WeaveFFI version, crate, generated target, or CLI command.
- A minimal reproducer or proof of concept.
- The expected impact, such as code execution, memory unsafety, data exposure,
  denial of service, or generated-code vulnerability.
- Any known mitigations or workarounds.

We will acknowledge receipt within 7 days and will keep you updated while we
investigate.

## Supported Versions

WeaveFFI is pre-1.0. Only the latest minor release receives security fixes.
Older minor releases may receive fixes at maintainer discretion, but consumers
should upgrade to the latest release before requesting backports.

| Version | Supported |
| ------- | --------- |
| Latest minor | Yes |
| Older minors | No |

## Coordinated Disclosure

We prefer coordinated disclosure. If you report a vulnerability privately, we
will work with you to confirm the issue, prepare a fix, publish patched crates
and binaries, and credit you in the advisory unless you prefer to remain
anonymous.

Please give us a reasonable opportunity to release a fix before publishing
details publicly.

## Disclosure Timeline

Our standard timeline is 90 days from acknowledgement to public disclosure.
That timeline may be shortened for active exploitation or extended by mutual
agreement when the fix requires ecosystem coordination.

Expected flow:

1. Acknowledge the report within 7 days.
2. Confirm impact and affected versions.
3. Prepare and test a fix.
4. Release patched versions and publish an advisory.
5. Publicly disclose technical details after the advisory is available.

## Scope

Security-sensitive areas include:

- The `weaveffi-abi` runtime and generated C ABI ownership rules.
- Generated bindings that manage pointers, callbacks, async contexts, or
  language-runtime handles.
- The parser, validator, and `weaveffi extract` input handling.
- CLI behavior that reads project files, runs hooks, or writes generated output.

Bug reports outside this scope are still welcome through normal GitHub issues.
