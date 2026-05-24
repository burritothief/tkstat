#!/usr/bin/env python3
"""Build or validate tkstat pricing catalog snapshots.

The main tkstat binary imports reviewed catalog JSON files without network
access. This maintainer script is intentionally separate: it can validate the
checked-in catalog, parse saved sanitized fixtures, or attempt a live fetch for
review. If an official page changes shape, the parser exits non-zero instead of
silently trusting partial data.
"""

from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import re
import sys
import urllib.request
from pathlib import Path
from typing import Any


ANTHROPIC_URL = "https://www.anthropic.com/pricing"
OPENAI_URL = "https://openai.com/api/pricing/"
SUPPORTED_DIMENSIONS = {
    "service_tier",
    "speed",
    "region",
    "processing_mode",
    "source_detail",
}


def positive_float(value: Any, field: str) -> float:
    try:
        parsed = float(value)
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{field} must be numeric") from exc
    if parsed < 0:
        raise ValueError(f"{field} must be non-negative")
    return parsed


def normalized_dimensions(value: Any) -> tuple[tuple[str, str], ...]:
    if value is None:
        return ()
    if not isinstance(value, dict):
        raise ValueError("dimensions must be an object")

    normalized: list[tuple[str, str]] = []
    for key, raw in sorted(value.items()):
        if key not in SUPPORTED_DIMENSIONS:
            raise ValueError(f"unsupported pricing dimension {key}")
        if raw is None:
            continue
        if not isinstance(raw, str):
            raise ValueError(f"pricing dimension {key} must be a string")
        dimension_value = raw.strip()
        if not dimension_value:
            raise ValueError(f"pricing dimension {key} must not be empty")
        normalized.append((key, dimension_value))
    return tuple(normalized)


def validate_catalog(catalog: dict[str, Any]) -> None:
    if catalog.get("schema_version") != 1:
        raise ValueError("schema_version must be 1")
    if not str(catalog.get("notes", "")).strip():
        raise ValueError("catalog notes must not be empty")

    source_ids: set[str] = set()
    for source in catalog.get("sources", []):
        source_id = str(source.get("id", "")).strip()
        if not source_id:
            raise ValueError("source id must not be empty")
        if source_id in source_ids:
            raise ValueError(f"duplicate source id {source_id}")
        source_ids.add(source_id)
        if not str(source.get("url", "")).startswith("https://"):
            raise ValueError(f"source {source_id} must use an https URL")
        dt.date.fromisoformat(str(source.get("retrieved_at", "")))
        if not str(source.get("notes", "")).strip():
            raise ValueError(f"source {source_id} notes must not be empty")

    if not source_ids:
        raise ValueError("catalog must contain at least one source")

    seen_keys: set[tuple[str, str, str, tuple[tuple[str, str], ...], str, str]] = set()
    for entry in catalog.get("entries", []):
        provider = str(entry.get("provider", "")).strip()
        if provider not in {"claude-code", "codex"}:
            raise ValueError(f"unsupported provider {provider}")
        if entry.get("source_ref") not in source_ids:
            raise ValueError(f"unknown source_ref {entry.get('source_ref')}")
        if not str(entry.get("source", "")).strip():
            raise ValueError("entry source must not be empty")
        if not str(entry.get("notes", "")).strip():
            raise ValueError("entry notes must not be empty")
        model_ids = entry.get("model_ids") or []
        if not model_ids or any(not str(model).strip() for model in model_ids):
            raise ValueError(f"entry for provider {provider} must contain model_ids")
        currency = str(entry.get("currency", "")).strip()
        if currency != "USD":
            raise ValueError("only USD catalog entries are supported")
        effective_from = str(entry.get("effective_from", ""))
        if not effective_from.endswith("+00:00"):
            raise ValueError("effective_from must use canonical UTC +00:00")
        rates = entry.get("rates_per_1m_tokens") or {}
        if not rates:
            raise ValueError("entry must contain rates_per_1m_tokens")
        dimensions = normalized_dimensions(entry.get("dimensions", {}))
        for category, rate in rates.items():
            if category not in {
                "input",
                "output",
                "cache_read",
                "cache_creation",
                "cached_input",
                "reasoning_output",
            }:
                raise ValueError(f"unsupported token category {category}")
            positive_float(rate, f"{provider}/{category}")
            for model_id in model_ids:
                key = (provider, model_id, category, dimensions, currency, effective_from)
                if key in seen_keys:
                    raise ValueError(f"duplicate catalog interval {key}")
                seen_keys.add(key)

    if not seen_keys:
        raise ValueError("catalog must expand to at least one interval")


def parse_anthropic_html(contents: str) -> list[dict[str, Any]]:
    rows = re.findall(r"<tr\b(?P<attrs>[^>]*)>", contents, flags=re.IGNORECASE)
    entries: list[dict[str, Any]] = []
    for attrs in rows:
        if "data-tkstat-provider=\"anthropic\"" not in attrs:
            continue
        values = dict(
            (name, html.unescape(value))
            for name, value in re.findall(r'(data-tkstat-[\w-]+)="([^"]*)"', attrs)
        )
        model_ids = [
            model.strip()
            for model in values.get("data-tkstat-model-ids", "").split(",")
            if model.strip()
        ]
        if not model_ids:
            raise ValueError("anthropic pricing row missing data-tkstat-model-ids")
        input_rate = positive_float(values.get("data-tkstat-input"), "anthropic input")
        output_rate = positive_float(values.get("data-tkstat-output"), "anthropic output")
        alias = values.get("data-tkstat-alias", "").strip()
        common = {
            "provider": "claude-code",
            "model_ids": model_ids,
            "model_aliases": [alias] if alias else [],
        }
        label = alias or model_ids[0]
        entries.append(
            {
                **common,
                "dimensions": {},
                "rates_per_1m_tokens": {
                    "input": input_rate,
                    "output": output_rate,
                    "cache_creation": round(input_rate * 1.25, 6),
                    "cache_read": round(input_rate * 0.10, 6),
                },
                "notes": f"Claude {label} pricing extracted for review.",
            }
        )
        entries.append(
            {
                **common,
                "dimensions": {"source_detail": "ephemeral_5m"},
                "rates_per_1m_tokens": {
                    "cache_creation": round(input_rate * 1.25, 6),
                },
                "notes": f"Claude {label} 5-minute prompt-cache write pricing extracted for review.",
            }
        )
        entries.append(
            {
                **common,
                "dimensions": {"source_detail": "ephemeral_1h"},
                "rates_per_1m_tokens": {
                    "cache_creation": round(input_rate * 2.0, 6),
                },
                "notes": f"Claude {label} 1-hour prompt-cache write pricing extracted for review.",
            }
        )
    if not entries:
        raise ValueError("no Anthropic pricing rows matched expected reviewed fixture shape")
    return entries


def parse_openai_json(contents: str) -> list[dict[str, Any]]:
    data = json.loads(contents)
    entries: list[dict[str, Any]] = []
    for row in data.get("models", []):
        model_ids = [str(model).strip() for model in row.get("model_ids", []) if str(model).strip()]
        if not model_ids:
            raise ValueError("openai pricing row missing model_ids")
        entries.append(
            {
                "provider": "codex",
                "model_ids": model_ids,
                "model_aliases": row.get("model_aliases", []),
                "dimensions": {"processing_mode": row.get("processing_mode", "standard")},
                "rates_per_1m_tokens": {
                    "input": positive_float(row.get("input"), "openai input"),
                    "cached_input": positive_float(row.get("cached_input"), "openai cached_input"),
                    "output": positive_float(row.get("output"), "openai output"),
                    "reasoning_output": positive_float(row.get("output"), "openai reasoning_output"),
                },
                "notes": str(row.get("notes", "OpenAI pricing extracted for review.")),
            }
        )
    if not entries:
        raise ValueError("no OpenAI pricing rows matched expected reviewed fixture shape")
    return entries


def catalog_from_sources(
    anthropic_html: str,
    openai_json: str,
    retrieved_at: str,
    effective_from: str,
) -> dict[str, Any]:
    entries: list[dict[str, Any]] = []
    for entry in parse_anthropic_html(anthropic_html):
        entry.update(
            {
                "currency": "USD",
                "effective_from": effective_from,
                "effective_to": None,
                "source": f"seed:anthropic-claude-pricing-{retrieved_at}",
                "source_ref": f"anthropic-claude-pricing-{retrieved_at}",
            }
        )
        entries.append(entry)
    for entry in parse_openai_json(openai_json):
        entry.update(
            {
                "currency": "USD",
                "effective_from": effective_from,
                "effective_to": None,
                "source": f"seed:openai-pricing-{retrieved_at}",
                "source_ref": f"openai-pricing-{retrieved_at}",
            }
        )
        entries.append(entry)

    catalog = {
        "schema_version": 1,
        "notes": "Bundled offline pricing snapshot generated for review.",
        "sources": [
            {
                "id": f"anthropic-claude-pricing-{retrieved_at}",
                "url": ANTHROPIC_URL,
                "retrieved_at": retrieved_at,
                "notes": "Generated from reviewed Anthropic pricing source.",
            },
            {
                "id": f"openai-pricing-{retrieved_at}",
                "url": OPENAI_URL,
                "retrieved_at": retrieved_at,
                "notes": "Generated from reviewed OpenAI pricing source.",
            },
        ],
        "entries": entries,
    }
    validate_catalog(catalog)
    return catalog


def fetch_url(url: str) -> str:
    request = urllib.request.Request(url, headers={"User-Agent": "tkstat-pricing-catalog-review"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read().decode("utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--validate-only", action="store_true", help="validate an existing catalog")
    mode.add_argument("--from-fixtures", action="store_true", help="build from saved sanitized inputs")
    mode.add_argument("--fetch", action="store_true", help="fetch official pages and build for review")
    parser.add_argument("--catalog", default="pricing/catalog.json", help="catalog path for --validate-only")
    parser.add_argument("--anthropic-html", help="saved Anthropic HTML snippet")
    parser.add_argument("--openai-json", help="saved OpenAI JSON snippet")
    parser.add_argument("--output", help="output catalog path")
    parser.add_argument(
        "--retrieved-at",
        default=dt.datetime.now(dt.UTC).date().isoformat(),
        help="source retrieval date YYYY-MM-DD",
    )
    parser.add_argument(
        "--effective-from",
        default=f"{dt.datetime.now(dt.UTC).date().isoformat()}T00:00:00+00:00",
        help="catalog interval effective_from in canonical UTC RFC3339",
    )
    args = parser.parse_args()

    try:
        if args.validate_only:
            catalog_path = Path(args.catalog)
            validate_catalog(json.loads(catalog_path.read_text()))
            print(f"pricing catalog OK: {catalog_path}")
            return 0

        if args.from_fixtures:
            if not args.anthropic_html or not args.openai_json or not args.output:
                raise ValueError("--from-fixtures requires --anthropic-html, --openai-json, and --output")
            catalog = catalog_from_sources(
                Path(args.anthropic_html).read_text(),
                Path(args.openai_json).read_text(),
                args.retrieved_at,
                args.effective_from,
            )
        else:
            if not args.output:
                raise ValueError("--fetch requires --output")
            catalog = catalog_from_sources(
                fetch_url(ANTHROPIC_URL),
                fetch_url(OPENAI_URL),
                args.retrieved_at,
                args.effective_from,
            )

        output = Path(args.output)
        output.write_text(json.dumps(catalog, indent=2) + "\n")
        print(f"wrote pricing catalog for review: {output}")
        return 0
    except Exception as exc:
        print(f"pricing catalog update failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
