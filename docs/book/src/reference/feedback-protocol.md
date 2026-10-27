# Feedback protocol

This page specifies how Helia sends feedback to the feedback relay, so that the
relay and any other client can be written against it. The Rust types are in the
`helia-feedback` crate (`protocol` and `pow` modules); the relay can depend on
that crate with `default-features = false` and use them directly, so the code and
this page cannot drift apart.

## Why there is a relay

Registration on the project's forge is closed, so people without an account
cannot open an issue there. The relay accepts a report from the app, checks it,
and files the issue as a bot account. The public relay is
`https://feedback.discworld.casa`. The app and `helia feedback` use it unless
told otherwise (see [Configuration](configuration.md#environment-variables)).

The relay is only ever contacted when a person presses **Send** in the app or
runs `helia feedback`. Helia has no telemetry and never contacts the relay on its
own.

## Overview

1. `GET /api/v1/challenge` returns a challenge.
2. The client finds a nonce that solves it (proof of work).
3. `POST /api/v1/feedback` sends the report, with the proof, and optionally a
   screenshot.
4. The relay answers with the new issue's number and address.

All requests are over HTTPS. A client accepts plain `http://` only for a loopback
address (tests and local development).

## `GET /api/v1/challenge`

Answer `200`:

```json
{
  "challenge": "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
  "difficulty": 20,
  "expires": "2026-10-19T20:15:00Z"
}
```

| Field | Meaning |
|---|---|
| `challenge` | 32 random bytes as 64 hexadecimal characters. |
| `difficulty` | The number of leading zero bits the hash must have. 20 takes a client about a million hashes, well under a second. |
| `expires` | An RFC 3339 time after which the relay refuses this challenge. |

A client refuses a difficulty above 28 rather than work for minutes.

## Proof of work

A nonce, an unsigned 64-bit integer, solves a challenge when

```text
SHA-256( challenge_bytes || nonce as 8 little-endian bytes )
```

has at least `difficulty` leading zero bits. `challenge_bytes` are the 32 bytes the
hexadecimal `challenge` decodes to, not the text. The client searches nonces from 0
upwards and sends the first that works.

The relay recomputes the hash, checks the difficulty it issued, checks that the
challenge was issued by it and has not expired, and accepts each challenge once.

**Test vector.** Challenge `000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f`
(the bytes 0 to 31):

| Difficulty | Smallest solving nonce | Digest |
|---|---|---|
| 12 | 1048 | `0008085f99feb36603e7f084fc76b5d4e781c672ec1b097e39b730fee1066e72` |
| 16 | 14417 | `000011acd5d18fd8abb6021f278532519f3226a075b84d2cbbcf9281dc06f13f` |

The same vector is a unit test in `helia-feedback` (`pow::tests`).

## `POST /api/v1/feedback`

`Content-Type: multipart/form-data`, with these parts:

| Part | Type | Required | Content |
|---|---|---|---|
| `report` | `application/json` | yes | The report, below. |
| `screenshot` | `image/png` | no | The clean screenshot, at most 5 MiB (5 242 880 bytes). The highlights are not drawn on it; the relay draws them from the JSON. |

### The report

```json
{
  "kind": "bug",
  "title": "The sharpening preview is blurry at 100%",
  "description": "I zoomed to 100% on the Sharpen step...",
  "app": {
    "name": "helia",
    "version": "0.3.0",
    "commit": "1a2b3c4d",
    "target": "x86_64-unknown-linux-gnu",
    "gpu": "Vulkan: Example GPU"
  },
  "os": "Debian GNU/Linux 13 (trixie) (x86_64)",
  "highlights": [{ "x": 1010, "y": 90, "w": 400, "h": 150 }],
  "pow": { "challenge": "0001...1e1f", "nonce": 14417 }
}
```

| Field | Type | Rules |
|---|---|---|
| `kind` | string | `bug`, `feature`, `question` or `other`. |
| `title` | string | 1 to 120 characters after trimming. |
| `description` | string | 0 to 8000 characters. When the sender chose to include system details this ends with a line `Open capture: <file name>, <width>x<height>, <bit depth>, <frames> frames`; never a folder path. |
| `app` | object or `null` | Present only when the sender chose to include system details. `gpu` may be `null`. `commit` ends in `+dirty` for a modified source tree. |
| `os` | string or `null` | As `app`. Never a host name or user name. |
| `highlights` | array | At most 32 rectangles, each with `w` and `h` above zero, in the pixels of the screenshot that is sent (the client rescales them if it had to shrink the image to fit 5 MiB). Empty when there is no screenshot. |
| `pow` | object | The `challenge` exactly as issued and the `nonce` found. Required by the relay today (see [Authentication](#authentication)). |

Unknown members are ignored. Every limit above is enforced by the relay as well as
by the client: the types and `Report::validate` in `helia-feedback` are the single
definition.

The relay is expected to file the issue with the title `[<Kind label>] <title>`
(`Bug`, `Feature request`, `Question`, `Other`), the description, a block of the
system details, the screenshot with the highlights drawn on it, and the label for
the kind. It should treat every field as untrusted text.

### Answers

| Status | Meaning | Body |
|---|---|---|
| `201` | Filed. | `{"issue_number": 81, "issue_url": "https://github.com/qisback/helia/issues/81"}` |
| `400` | The report breaks a rule, or the proof of work is wrong, expired or used. | `{"error": {"code": "...", "message": "..."}}` |
| `413` | The request or the screenshot is too large. | the same error object |
| `415` | A part has the wrong media type (the screenshot is not a PNG). | the same error object |
| `429` | Too many reports. A `Retry-After` header gives the wait in seconds. | any |
| `503` | The relay is degraded (for example, it cannot reach the forge). | any |

`code` is a short machine-readable word such as `title_too_long`, `bad_pow` or
`screenshot_too_large`; `message` is for people and is shown to the sender.

## What the client does when this fails

The same 429, 503, connection failures and anything unexpected all lead to the
same place: the app tells the sender what happened and offers **Save report** (a
`.zip` with `report.json`, `screenshot.png` with the highlights drawn on, and
`screenshot-clean.png`) and **Open in browser** (the forge's new-issue page with
the title and text filled in, for people who have an account). `helia feedback`
prints the same address and, with `--save`, writes the zip. See
[Sending feedback](../user-guide/sending-feedback.md).

## Authentication

How a submission proves it is not a flood is deliberately separate from the report
format. In the client it sits behind one small interface, `RelayAuth` in
`helia_feedback::client`: it supplies the HTTP agent and prepares an `AuthProof`
that goes into the report.

| Method | State | How |
|---|---|---|
| Proof of work | Implemented | The challenge and nonce above, in the `pow` member. |
| Mutual TLS | Planned | The project will eventually have an API that accepts it with a TLS client certificate. A client presents the certificate when it connects; no proof of work is needed, so `pow` is absent from the report. |

Adding the planned method means adding another `RelayAuth` implementation; the
feedback page, the command and the report format do not change. It is not
implemented yet, and a relay should keep requiring `pow` until it is.

## Versioning

The path carries the version (`/api/v1/`). A change that is not backwards
compatible gets a new path; adding an optional member to the report is
compatible.
