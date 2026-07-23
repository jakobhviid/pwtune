# Thin shortcuts over speaker-eq.py — the Python CLI is the actual tool; these
# recipes just save you typing `python3 speaker-eq.py ...`. Everything here works
# the same run directly, e.g. `python3 speaker-eq.py measure`.

py := "python3 " + justfile_directory() + "/speaker-eq.py"

# Show available recipes
default:
    @just --list

# Play a sweep and print the frequency response (no changes). Extra flags pass through, e.g. `just measure --mic <src>`
measure *args:
    {{py}} measure {{args}}

# Measure + generate a starting profile -> profiles/<name>.conf. e.g. `just create desk-monitor --boost 12`
create name *args:
    {{py}} create {{name}} {{args}}

# List profiles and show which are installed / active
list:
    {{py}} list

# Install a profile (prompts if no name given)
install name="":
    {{py}} install {{name}}

# Remove an installed profile (prompts if no name given)
uninstall name="":
    {{py}} uninstall {{name}}

# Copy a finished profile into a sibling ReinstallScripts checkout (../ReinstallScripts)
promote name:
    {{py}} promote {{name}}
