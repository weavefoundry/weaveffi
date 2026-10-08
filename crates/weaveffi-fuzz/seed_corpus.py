#!/usr/bin/env python3
"""Seed a fuzz target's corpus from its committed seeds and the snapshot fixtures.

Usage: seed_corpus.py <target> <corpus-dir>

Copies crates/weaveffi-fuzz/fuzz/seeds/<target>/ into <corpus-dir>, then adds
inputs derived from crates/weaveffi-cli/tests/fixtures/*.yml, which between
them cover the whole IDL surface:

- fuzz_parse_yaml, fuzz_validate: each fixture as is
- fuzz_parse_json: each fixture converted to JSON
- fuzz_parse_toml: each fixture converted to TOML (needs `tomli-w`)
- fuzz_parse_type_ref: every distinct `type` and `return` string
- fuzz_value_buffer: the committed seeds only

Needs PyYAML (`pip install pyyaml tomli-w`).
"""

import json
import pathlib
import shutil
import sys

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
SEEDS = ROOT / "crates/weaveffi-fuzz/fuzz/seeds"
FIXTURES = sorted((ROOT / "crates/weaveffi-cli/tests/fixtures").glob("*.yml"))


def type_strings(node, out):
    """Collect every string under a `type` or `return` key."""
    if isinstance(node, dict):
        for key, value in node.items():
            if key in ("type", "return") and isinstance(value, str):
                out.add(value)
            type_strings(value, out)
    elif isinstance(node, list):
        for item in node:
            type_strings(item, out)


class FixtureLoader(yaml.SafeLoader):
    """Reads a plain `null` as the string "null", as the IDL parser does where
    it expects a string (`edge_cases` has a parameter named `null`). The
    fixtures use no real nulls, which TOML couldn't express anyway."""


FixtureLoader.yaml_implicit_resolvers = {
    first: [(tag, regex) for tag, regex in resolvers if tag != "tag:yaml.org,2002:null"]
    for first, resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items()
}


def load(fixture):
    return yaml.load(fixture.read_text(), Loader=FixtureLoader)


def main():
    target, corpus = sys.argv[1], pathlib.Path(sys.argv[2])
    corpus.mkdir(parents=True, exist_ok=True)
    for seed in (SEEDS / target).iterdir():
        shutil.copy(seed, corpus / seed.name)

    if target in ("fuzz_parse_yaml", "fuzz_validate"):
        for fixture in FIXTURES:
            shutil.copy(fixture, corpus / fixture.name)
    elif target == "fuzz_parse_json":
        for fixture in FIXTURES:
            text = json.dumps(load(fixture), indent=2)
            (corpus / f"{fixture.stem}.json").write_text(text)
    elif target == "fuzz_parse_toml":
        import tomli_w

        for fixture in FIXTURES:
            (corpus / f"{fixture.stem}.toml").write_text(tomli_w.dumps(load(fixture)))
    elif target == "fuzz_parse_type_ref":
        types = set()
        for fixture in FIXTURES:
            type_strings(load(fixture), types)
        for i, ty in enumerate(sorted(types)):
            (corpus / f"fixture_type_{i:03}.txt").write_text(ty)
    elif target != "fuzz_value_buffer":
        sys.exit(f"unknown fuzz target: {target}")

    print(f"{target}: {len(list(corpus.iterdir()))} corpus inputs")


if __name__ == "__main__":
    main()
