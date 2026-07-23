# Shortcuts over speaker-eq.py — the Python CLI is the actual tool; these recipes
# just save typing and guide you. Every verb works with no arguments: it lists
# what's available, defaults to the sensible choice for this machine, and prompts
# for the rest. You don't need to remember device names or flags.
#
# `measure` and `create` need NumPy/SciPy; the first time you run either, a local
# .venv is created and the libraries installed automatically. The other verbs
# don't need them and run on the system python3.

script := justfile_directory() + "/speaker-eq.py"
venv    := justfile_directory() + "/.venv"
vpy     := venv + "/bin/python"

# So the tool's "Next:" hints show `just …` (how you're running it) rather than raw python
export SPEAKER_EQ_LAUNCHER := "just"

# Show available commands
default:
    @just --list

# (internal) create the local Python env with numpy+scipy on first use
_deps:
    #!/usr/bin/env bash
    set -e
    if [[ ! -x "{{vpy}}" ]]; then
        echo "First run: setting up a local Python environment (numpy + scipy)…"
        python3 -m venv "{{venv}}"
        "{{vpy}}" -m pip install -q --upgrade pip
        "{{vpy}}" -m pip install -q numpy scipy
        echo "…done."
    fi

# Play a sweep and show the frequency response (guides you through speaker+mic; no changes)
measure *args: _deps
    "{{vpy}}" "{{script}}" measure {{args}}

# Measure and generate a starting profile (prompts for name, boost, speaker, mic)
create *args: _deps
    "{{vpy}}" "{{script}}" create {{args}}

# List profiles and show which are installed / which is the active output
list:
    python3 "{{script}}" list

# Install a profile as an EQ output (no name → pick from a list, defaults to the connected speaker)
install name="":
    python3 "{{script}}" install {{name}}

# Remove an installed profile (no name → pick from installed ones)
uninstall name="":
    python3 "{{script}}" uninstall {{name}}

# Copy a finished profile into ReinstallScripts (clones it via SSH if missing)
promote name="":
    python3 "{{script}}" promote {{name}}
