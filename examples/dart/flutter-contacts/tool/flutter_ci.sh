#!/usr/bin/env bash
set -euo pipefail

if ! command -v flutter >/dev/null 2>&1; then
  echo "Flutter SDK not found; skipping optional Flutter contacts example."
  exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
cd "$repo_root"

cargo build -p contacts
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target dart

cd examples/dart/flutter-contacts
flutter pub get
flutter analyze
flutter test
flutter build bundle
