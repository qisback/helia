# Scripting with the API

**Goal:** drive Helia from your own Python, a notebook or any program that can
speak HTTP.

The server is the `helia serve` command. It runs the same pipeline as `helia stack`
with the same options. The guide is [The API](../user-guide/api.md) and the full
list of operations is the [API reference](../reference/api.md).

## 1. Start the server

```sh
helia serve --data-root ~/captures
```

```text
listening on http://127.0.0.1:7870
data root: /home/you/captures
warning: no token set; any local process can read files under the data root
```

* Every path in a request is relative to `--data-root`, and cannot escape it.
* It listens on the loopback interface only. To listen on another address you
  must set a token (`--token-file`, or `HELIA_TOKEN`); see the
  [security model](../user-guide/api.md#security-model).

## 2. A complete script

This script, which uses only the Python standard library, queues a job, watches its
progress and downloads the result. It was run against the released v0.2.0 for this
manual.

```python
#!/usr/bin/env python3
"""Stack a capture through a running `helia serve` and download the result.

    helia serve --data-root ~/captures
    python stack_via_api.py sun.ser stacks/sun.tif --preset ha --sharpen wavelet

Standard library only. Paths are relative to the server's data root.
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("HELIA_URL", "http://127.0.0.1:7870/api/v1")
TOKEN = os.environ.get("HELIA_TOKEN")  # only if the server was started with one


def call(method, path, body=None, raw=False):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req) as resp:
            payload = resp.read()
    except urllib.error.HTTPError as err:
        detail = json.loads(err.read() or b"{}").get("error", {})
        sys.exit(f"{method} {path}: {err.code} {detail.get('code')}: {detail.get('message')}")
    return payload if raw else json.loads(payload)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("input", help="capture, relative to the data root")
    ap.add_argument("output", help="result, relative to the data root (.tif, .png or .fits)")
    ap.add_argument("--preset", help="filter id: ha, cak, cah, mg, na, he-d3, hbeta, white")
    ap.add_argument("--sharpen", choices=["wavelet", "unsharp", "rl", "none"])
    ap.add_argument("--keep", help="share of frames to keep, e.g. 20%%")
    args = ap.parse_args()

    config = {k: v for k, v in {
        "preset": args.preset, "sharpen": args.sharpen, "keep": args.keep,
    }.items() if v}
    job = call("POST", "/jobs", {
        "input": args.input, "output": args.output,
        "config": config, "overwrite": True,
    })
    print("queued job", job["id"])

    while job["state"] in ("queued", "running"):
        time.sleep(0.5)
        job = call("GET", f"/jobs/{job['id']}")
        print(f"  {job['state']:9} {job.get('stage') or '':24} {job['progress']:4.0%}")

    if job["state"] != "succeeded":
        sys.exit(f"job {job['state']}: {job.get('error')}")

    local = os.path.basename(args.output)
    with open(local, "wb") as f:
        f.write(call("GET", f"/jobs/{job['id']}/result", raw=True))
    print("saved", local)


if __name__ == "__main__":
    main()
```

Run on a 100-frame capture, it prints:

```text
queued job 1
  running   opening                    0%
  running   reference                 25%
  running   global_align              31%
  running   local_align               65%
  running   stack                     95%
  succeeded write                    100%
saved disc.tif
```

## 3. Things you can ask

* `GET /health` and `GET /version`: is it up, and what is it (version, commit,
  backend, limits).
* `GET /presets`: the filter presets and their colours.
* `POST /captures/inspect`: what a file contains, with the same notices `helia info`
  prints (for example the Bayer notice).
* `GET /captures/frame`: one decoded frame as an image, for your own viewer.
* `POST /jobs`, `GET /jobs/{id}`, `DELETE /jobs/{id}`: queue, watch and cancel.
* `GET /jobs/{id}/preview.png` and `/result`: a PNG preview and the file itself.
* `GET /jobs/{id}/events`: a WebSocket stream of progress, instead of polling.
* `GET /openapi.json` and `GET /docs`: the machine-readable description and a
  small browsable page, served by the running server.

A job's `config` takes the same choices as the command line: `keep`, `ap_size`,
`preset`, `sharpen`, `edits`, `colour` and so on. The errors are always
`{"error": {"code": ..., "message": ...}}` with a stable `code`, which is what the
script above prints. See [The API](../user-guide/api.md#post-jobs).

## 4. Notebooks and other languages

Nothing here is Python-specific: it is JSON over HTTP, and a WebSocket for events.
In a notebook, replace `urllib` with `requests` if you have it; the
[API guide](../user-guide/api.md#python) shows that form.
