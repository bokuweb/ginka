# Security

## Reporting a vulnerability

Please report privately, through GitHub's private vulnerability reporting on
[bokuweb/ginka](https://github.com/bokuweb/ginka/security) (*Report a
vulnerability*). If that is not available, open an issue asking for a
private channel — without the details.

Include what an attacker needs, what they get, and the smallest way to
reproduce it. There is no release yet, so fixes land on `main`.

## What Ginka is designed to hold

The model is a single user on their own machine (`docs/roadmap.md` §6.3):

- The daemon binds loopback only. Clients present a bearer token from a
  handshake file written `0600`; the token never appears in logs.
- Agents run with a sanitized environment: session state inherited from
  whatever started the daemon is removed, and a user's gateways and keys are
  set per agent in `settings.json` instead.
- Ginka reads no credential and performs no login. An account is a directory
  the vendor's own CLI signs into; the wire carries the names of an
  account's environment variables, never their values.
- Paths from the daemon are paths on the daemon's host, and a client that is
  not on that host does not treat them as its own.
- Chat connectors deny every sender not on an allowlist, keep tokens out of
  the database and the logs, and only dial out.
- Nothing is sent anywhere unasked: no telemetry, and crash reports are
  files in the logs directory. The only network requests Ginka makes itself
  are the ones a feature names — the agents' own, `gh` for pull requests,
  a connector's platform, and the public rate table, which can be turned off
  (`fetch_rates`).

A report that any of these does not hold is a vulnerability.
