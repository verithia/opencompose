#!/usr/bin/env python3
"""Refresh the bundled AI model price snapshot.

Prices come from the models.dev dataset (https://models.dev/api.json), which
publishes, per provider and model, the input, output and cached-input price in
USD per million tokens. This script keeps only the (provider, model) pairs the
bundled catalogue (crates/oc-gui/frontend/src-tauri/ai-catalog.json) declares,
so the snapshot stays small and never prices a model we cannot request.

A model the dataset does not price gets no record. The panel then shows no cost
for it instead of $0.00, and a user price in the model limits dialog wins over
whatever this file says.

Usage:
    python3 scripts/refresh-ai-prices.py
    python3 scripts/refresh-ai-prices.py --source /tmp/api.json
    python3 scripts/refresh-ai-prices.py --check

`--check` fetches the dataset, rebuilds the snapshot in memory and exits 1 when
the committed file differs, so a stale snapshot is visible in review.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import urllib.request
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CATALOG = ROOT / "crates/oc-gui/frontend/src-tauri/ai-catalog.json"
SNAPSHOT = ROOT / "crates/oc-gui/frontend/src-tauri/ai-prices.json"
SOURCE = "https://models.dev/api.json"

# Our provider id -> the models.dev provider that publishes the same per-token
# price. `custom` and `ollama` have no entry: a user's own endpoint and a local
# server are not billed per token, so they stay unpriced.
PROVIDERS = {
    "openai": "openai",
    "anthropic": "anthropic",
    "google": "google",
    "opencode-go": "opencode-go",
    "deepseek": "deepseek",
}


def fetch(url: str) -> tuple[bytes, dict]:
    with urllib.request.urlopen(url, timeout=60) as response:
        body = response.read()
    return body, json.loads(body)


def records(dataset: dict, catalog: dict) -> list[dict]:
    found = []
    for provider in catalog["providers"]:
        source_id = PROVIDERS.get(provider["id"])
        if source_id is None or source_id not in dataset:
            continue
        published = dataset[source_id].get("models", {})
        for model in provider.get("models", []):
            cost = published.get(model["id"], {}).get("cost")
            # A model with no cost object is unknown, not free: skip it.
            if not isinstance(cost, dict) or "input" not in cost or "output" not in cost:
                continue
            record = {
                "provider": provider["id"],
                "model": model["id"],
                "input": cost["input"],
                "output": cost["output"],
                "currency": "USD",
            }
            if isinstance(cost.get("cache_read"), (int, float)):
                record["cached_input"] = cost["cache_read"]
            found.append(record)
    found.sort(key=lambda record: (record["provider"], record["model"]))
    return found


def snapshot(dataset: dict, catalog: dict, digest: str) -> dict:
    return {
        "version": date.today().isoformat(),
        "source": SOURCE,
        "source_sha256": digest,
        "unit": "USD per million tokens",
        "records": records(dataset, catalog),
    }


def render(snapshot_data: dict) -> str:
    return json.dumps(snapshot_data, indent=2) + "\n"


def unpriced(catalog: dict, snapshot_data: dict) -> list[str]:
    priced = {(record["provider"], record["model"]) for record in snapshot_data["records"]}
    return [
        f"{provider['id']}/{model['id']}"
        for provider in catalog["providers"]
        for model in provider.get("models", [])
        if (provider["id"], model["id"]) not in priced
    ]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--source", default=SOURCE, help="dataset URL or a local JSON file")
    parser.add_argument("--check", action="store_true", help="fail when the committed snapshot is stale")
    parser.add_argument("--output", type=Path, default=SNAPSHOT, help="snapshot path")
    args = parser.parse_args(argv)

    if args.source.startswith(("http://", "https://")):
        body, dataset = fetch(args.source)
    else:
        body = Path(args.source).read_bytes()
        dataset = json.loads(body)
    catalog = json.loads(CATALOG.read_text())
    text = render(snapshot(dataset, catalog, hashlib.sha256(body).hexdigest()))

    if args.check:
        current = args.output.read_text() if args.output.exists() else ""
        if current != text:
            print(f"{args.output} is stale; run scripts/refresh-ai-prices.py", file=sys.stderr)
            return 1
        print(f"{args.output} matches the dataset")
        return 0

    args.output.write_text(text)
    data = json.loads(text)
    print(
        f"wrote {len(data['records'])} prices to {args.output} "
        f"({len(unpriced(catalog, data))} catalog models have no published price)"
    )
    for entry in unpriced(catalog, data):
        print(f"  unpriced: {entry}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
