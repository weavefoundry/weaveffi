#!/usr/bin/env bash
# Interpreter discovery shared by the fixture checks (scripts/fixtures), the
# conformance lanes (conformance/), and scripts/package-smoke.sh.
#
# The generated Python package requires Python 3.10 or newer and the
# generated gem Ruby 3.2 or newer, while a system `python3` or `ruby` can be
# older (macOS ships Python 3.9 and Ruby 2.6). These helpers respect the
# interpreter on PATH when it's new enough and otherwise look for a newer
# one under a versioned name (Python) or in a Homebrew keg (Ruby).

# Print the command of the first Python 3.10+ interpreter found: `python3`
# on PATH first, then `python3.N` names from newest to oldest. Fail when
# there's none.
find_python() {
    local candidate
    for candidate in python3 python3.14 python3.13 python3.12 python3.11 python3.10 python; do
        command -v "$candidate" >/dev/null 2>&1 || continue
        if "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 10))' 2>/dev/null; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

# Put a Ruby 3.2+ first on PATH, with its own `gem`: the `ruby` on PATH when
# it's new enough, else Homebrew's keg-only Ruby. Fail when there's none.
use_ruby() {
    local candidate dir
    for candidate in "$(command -v ruby 2>/dev/null)" \
        "$(brew --prefix ruby 2>/dev/null)/bin/ruby"; do
        [ -n "$candidate" ] && [ -x "$candidate" ] || continue
        if "$candidate" -e 'exit(Gem::Version.new(RUBY_VERSION) >= Gem::Version.new("3.2"))' 2>/dev/null; then
            dir=$(dirname "$candidate")
            case ":$PATH:" in
                ":$dir:"*) ;;
                *) PATH="$dir:$PATH"; export PATH ;;
            esac
            return 0
        fi
    done
    return 1
}
