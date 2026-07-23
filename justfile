# Thin shortcuts over the `pwtune` binary — the binary is the tool; these just
# save typing and build it on first use. Building needs Rust (cargo); after that
# it's a single self-contained binary with no runtime deps beyond PipeWire.
#
# Every verb runs with no arguments too: it enumerates what's available, defaults
# to the sensible choice for this machine, and prompts for the rest.

export PWTUNE_LAUNCHER := "just"
export PWTUNE_HOME := justfile_directory()
bin := justfile_directory() + "/target/release/pwtune"

# Show available recipes
default:
    @just --list

# Build the release binary (recompiles only when sources change)
build:
    cargo build --release --manifest-path "{{justfile_directory()}}/Cargo.toml"

_build:
    @cargo build --release --quiet --manifest-path "{{justfile_directory()}}/Cargo.toml"

# Play a sweep and print the frequency response (no changes)
measure *args: _build
    "{{bin}}" measure {{args}}

# Measure + generate a starting profile -> drafts/<name>.conf (prompts for name/boost)
create *args: _build
    "{{bin}}" create {{args}}

# List profiles (draft + shipped) and their install state
list: _build
    "{{bin}}" list

# Install a profile as an EQ output (no name → pick, defaults to the connected speaker)
install name="": _build
    "{{bin}}" install {{name}}

# Edit a draft profile in $EDITOR (calibrated ones are frozen)
edit name="": _build
    "{{bin}}" edit {{name}}

# Delete a draft profile (calibrated ones are frozen)
delete name="": _build
    "{{bin}}" delete {{name}}

# Remove a deployed EQ (no name → sectioned pick; also lists orphaned/foreign configs)
uninstall name="": _build
    "{{bin}}" uninstall {{name}}

# Finalize a draft: move it into calibrated/ (shipped, frozen)
promote name="": _build
    "{{bin}}" promote {{name}}
