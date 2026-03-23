#!/usr/bin/env python3
"""Tests for normalize_apify_following.py"""
import json
import pathlib
import subprocess
import sys
import tempfile
from typing import Any, Dict, List

import pytest


SCRIPT = pathlib.Path(__file__).parent / "normalize_apify_following.py"


def run_normalizer(input_data: List[Dict[str, Any]], extra_args: List[str] | None = None) -> List[Dict[str, Any]]:
    with tempfile.NamedTemporaryFile(mode="w", suffix=".jsonl", delete=False) as f:
        for row in input_data:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
        input_path = pathlib.Path(f.name)

    output_path = input_path.with_suffix(".json")
    try:
        cmd = [sys.executable, str(SCRIPT), "--input-jsonl", str(input_path), "--output", str(output_path)]
        if extra_args:
            cmd.extend(extra_args)
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return json.loads(output_path.read_text(encoding="utf-8"))
    finally:
        input_path.unlink(missing_ok=True)
        output_path.unlink(missing_ok=True)


CANONICAL_KEYS = {
    "username",
    "display_name",
    "bio",
    "profile_url",
    "follows_you",
    "verified",
    "protected",
    "button_text",
    "avatar_url",
    "card_text",
    "captured_at",
    "followers_count",
    "following_count",
    "collection_source",
}


# --- Actor format fixtures ---

APIDOJO_ROW = {
    "screen_name": "levelsio",
    "name": "Pieter Levels",
    "description": "Building startups in public",
    "followers_count": 792000,
    "friends_count": 1234,
    "verified": True,
    "profile_image_url": "https://pbs.twimg.com/levelsio.jpg",
}

MICROWORLDS_ROW = {
    "userName": "mckaywrigley",
    "displayName": "McKay Wrigley",
    "bio": "AI builder",
    "followersCount": 150000,
    "followingCount": 500,
    "isVerified": False,
    "profileUrl": "https://x.com/mckaywrigley",
    "profileImageUrl": "https://pbs.twimg.com/mckay.jpg",
}

WEB_SCRAPING_PRO_ROW = {
    "username": "naval",
    "name": "Naval Ravikant",
    "description": "Angel investor",
    "followers": 2100000,
    "following": 300,
    "verified": True,
    "url": "https://x.com/naval",
    "avatar": "https://pbs.twimg.com/naval.jpg",
}


class TestApidojoFormat:
    def test_field_mapping(self):
        rows = run_normalizer([APIDOJO_ROW])
        assert len(rows) == 1
        row = rows[0]
        assert row["username"] == "levelsio"
        assert row["display_name"] == "Pieter Levels"
        assert row["bio"] == "Building startups in public"
        assert row["followers_count"] == 792000
        assert row["following_count"] == 1234
        assert row["verified"] is True
        assert row["avatar_url"] == "https://pbs.twimg.com/levelsio.jpg"
        assert row["profile_url"] == "https://x.com/levelsio"

    def test_cdp_absent_fields_have_defaults(self):
        rows = run_normalizer([APIDOJO_ROW])
        row = rows[0]
        assert row["follows_you"] is False
        assert row["protected"] is False
        assert row["button_text"] == ""
        assert row["card_text"] == ""
        assert row["collection_source"] == "apify"


class TestMicroworldsFormat:
    def test_field_mapping(self):
        rows = run_normalizer([MICROWORLDS_ROW])
        assert len(rows) == 1
        row = rows[0]
        assert row["username"] == "mckaywrigley"
        assert row["display_name"] == "McKay Wrigley"
        assert row["bio"] == "AI builder"
        assert row["followers_count"] == 150000
        assert row["following_count"] == 500
        assert row["verified"] is False
        assert row["avatar_url"] == "https://pbs.twimg.com/mckay.jpg"
        assert row["profile_url"] == "https://x.com/mckaywrigley"


class TestWebScrapingProFormat:
    def test_field_mapping(self):
        rows = run_normalizer([WEB_SCRAPING_PRO_ROW])
        assert len(rows) == 1
        row = rows[0]
        assert row["username"] == "naval"
        assert row["display_name"] == "Naval Ravikant"
        assert row["bio"] == "Angel investor"
        assert row["followers_count"] == 2100000
        assert row["following_count"] == 300
        assert row["verified"] is True
        assert row["avatar_url"] == "https://pbs.twimg.com/naval.jpg"
        assert row["profile_url"] == "https://x.com/naval"


class TestCanonicalSchema:
    def test_all_actors_produce_canonical_keys(self):
        rows = run_normalizer([APIDOJO_ROW, MICROWORLDS_ROW, WEB_SCRAPING_PRO_ROW])
        for row in rows:
            assert set(row.keys()) == CANONICAL_KEYS, f"Key mismatch for {row.get('username')}: {set(row.keys()) ^ CANONICAL_KEYS}"

    def test_captured_at_is_iso_format(self):
        rows = run_normalizer([APIDOJO_ROW])
        import datetime
        # Should not raise
        datetime.datetime.fromisoformat(rows[0]["captured_at"])


class TestDeduplication:
    def test_duplicate_usernames_keep_first(self):
        dup1 = {**APIDOJO_ROW, "screen_name": "TestUser", "description": "first"}
        dup2 = {**APIDOJO_ROW, "screen_name": "testuser", "description": "second"}
        rows = run_normalizer([dup1, dup2])
        assert len(rows) == 1
        assert rows[0]["bio"] == "first"

    def test_mixed_actor_formats_dedup(self):
        # Same user from different actors
        a = {**APIDOJO_ROW, "screen_name": "shared_user"}
        b = {**MICROWORLDS_ROW, "userName": "Shared_User"}
        rows = run_normalizer([a, b])
        assert len(rows) == 1


class TestEdgeCases:
    def test_missing_optional_fields(self):
        minimal = {"screen_name": "minuser"}
        rows = run_normalizer([minimal])
        assert len(rows) == 1
        row = rows[0]
        assert row["username"] == "minuser"
        assert row["display_name"] == "minuser"
        assert row["bio"] == ""
        assert row["followers_count"] is None
        assert row["following_count"] is None
        assert row["verified"] is False

    def test_null_values(self):
        row_with_nulls = {
            "screen_name": "nulltest",
            "name": None,
            "description": None,
            "followers_count": None,
            "verified": None,
        }
        rows = run_normalizer([row_with_nulls])
        assert len(rows) == 1
        row = rows[0]
        assert row["display_name"] == "nulltest"
        assert row["bio"] == ""
        assert row["followers_count"] is None
        assert row["verified"] is False

    def test_invalid_username_skipped(self):
        invalid = {"screen_name": "", "name": "No Handle"}
        rows = run_normalizer([invalid])
        assert len(rows) == 0

    def test_at_prefix_stripped(self):
        with_at = {**MICROWORLDS_ROW, "userName": "@mckaywrigley"}
        rows = run_normalizer([with_at])
        assert rows[0]["username"] == "mckaywrigley"

    def test_empty_input(self):
        rows = run_normalizer([])
        assert rows == []

    def test_json_array_input(self):
        """Input can also be a plain JSON array file (not JSONL)."""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump([APIDOJO_ROW, MICROWORLDS_ROW], f, ensure_ascii=False)
            input_path = pathlib.Path(f.name)
        output_path = input_path.with_suffix(".out.json")
        try:
            subprocess.run(
                [sys.executable, str(SCRIPT), "--input-jsonl", str(input_path), "--output", str(output_path)],
                capture_output=True, text=True, check=True,
            )
            rows = json.loads(output_path.read_text(encoding="utf-8"))
            assert len(rows) == 2
        finally:
            input_path.unlink(missing_ok=True)
            output_path.unlink(missing_ok=True)


class TestSorting:
    def test_output_sorted_by_username(self):
        rows = run_normalizer([WEB_SCRAPING_PRO_ROW, APIDOJO_ROW, MICROWORLDS_ROW])
        usernames = [r["username"] for r in rows]
        assert usernames == sorted(usernames, key=str.lower)
