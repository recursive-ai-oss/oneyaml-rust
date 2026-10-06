# Security policy

oneyaml parses input that often comes from somewhere you don't control.
Its design goals include no `unsafe` code, no panics on any input, and
resource limits (`Limits::untrusted()`) that bound memory and time on
hostile input. A way to break any of those is a security issue, and we
want to hear about it.

## Supported versions

| Crate | Supported |
|---|---|
| `oneyaml` 1.x | yes: the latest 1.x release |
| `oneyaml-core` 1.x | yes: the latest 1.x release |
| `oneyaml-syntax` | the latest release only (its own semver track) |
| any 0.x release | no: upgrade to 1.x |

Fixes are released as a patch of the latest minor version. Security
fixes are the one case where `Limits::default()` may be tightened within
a major version.

## Reporting a vulnerability

**Don't open a public issue, pull request or discussion.** Report it
privately through GitHub's private vulnerability reporting:

1. Go to <https://github.com/recursive-ai-oss/oneyaml-rust/security/advisories/new>
   (the **Security** tab of the repository, then **Report a
   vulnerability**).
2. Describe the problem, the affected crate and version, and the input
   or code that shows it. The smallest input that reproduces it helps
   most.

Only the maintainers can see the report. You can expect:

- an acknowledgement within 7 days;
- a fix, or a plan with a timeline, after we have confirmed the issue;
- a GitHub security advisory and a RustSec advisory once a fix is
  released, crediting you unless you ask us not to.

Please give us the chance to release a fix before you disclose the issue
publicly.

## What counts

Examples of what we treat as security issues:

- a panic, abort or stack overflow on any input, or from any public API
  call;
- memory or time far beyond the input's size under `Limits::untrusted()`
  (for example an alias expansion that escapes the alias budgets);
- input that `oneyaml`'s serde layer and its `Document` read with
  different meanings, if it can be used to slip content past a check;
- output from the serializer that reads back as a different value.

Wrong results on valid YAML that have no security impact are ordinary
bugs: please open an issue for those.
