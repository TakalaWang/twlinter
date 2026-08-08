#!/usr/bin/env bash
# Deploy script for zhtw-mcp: install, uninstall, status.
#
# zhtw-mcp is a long-running MCP server managed by MCP-capable agents.
# The running process must be killed before overwriting the binary.

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

BINARY_NAME="zhtw-mcp"
CODEX_MCP_NAME="${CODEX_MCP_NAME:-zhtw}"

print_info()   { echo -e "${GREEN}[INFO]${NC}   $1"; }
print_warn()   { echo -e "${YELLOW}[WARN]${NC}   $1"; }
print_error()  { echo -e "${RED}[ERROR]${NC}  $1"; }

# Escape ERE metacharacters so an absolute path can be safely embedded in a
# regex (e.g., for pgrep -f). Without this, dots in '~/.local' or other
# metacharacters in the user's home directory would match unrelated processes.
escape_ere() {
    printf '%s' "$1" | sed 's/[].[^$*+?(){}|\\]/\\&/g'
}
print_status() { echo -e "${BLUE}[STATUS]${NC} $1"; }

# Resolve project root relative to this script, regardless of CWD.
get_script_dir() {
    local source="${BASH_SOURCE[0]}"
    while [ -h "$source" ]; do
        local dir
        dir="$(cd -P "$(dirname "$source")" && pwd)"
        source="$(readlink "$source")"
        [[ $source != /* ]] && source="$dir/$source"
    done
    cd -P "$(dirname "$source")" && pwd
}

PROJECT_ROOT="$(cd "$(get_script_dir)/.." && pwd)"

# --- helpers ------------------------------------------------------------------

detect_install_dir() {
    if [ -n "${XDG_BIN_HOME:-}" ]; then
        echo "$XDG_BIN_HOME"
    else
        echo "$HOME/.local/bin"
    fi
}

ensure_install_dir() {
    local dir="$1"
    if [ ! -d "$dir" ]; then
        print_info "Creating install directory: $dir"
        mkdir -p "$dir"
    fi
}

check_path() {
    local install_dir="${1%/}"
    case ":$PATH:" in
        *":$install_dir:"*)
            print_info "Install directory is in PATH"
            ;;
        *)
            print_warn "Install directory is not in PATH"
            echo "  Add to your shell profile: export PATH=\"$install_dir:\$PATH\""
            ;;
    esac
}

check_claude_cli() {
    if ! command -v claude &>/dev/null; then
        print_warn "Claude CLI not found in PATH"
        echo "  Install Claude Code: npm install -g @anthropic-ai/claude-code"
        return 1
    fi
    print_info "Claude CLI found: $(which claude)"
    return 0
}

check_codex_cli() {
    if ! command -v codex &>/dev/null; then
        print_warn "Codex CLI not found in PATH"
        echo "  Install Codex CLI before using Codex MCP registration."
        return 1
    fi
    print_info "Codex CLI found: $(which codex)"
    return 0
}

# Kill running zhtw-mcp processes by exact installed binary path so we don't
# accidentally kill unrelated processes (cargo, editors, log tailers) whose
# argv happens to contain the string "zhtw-mcp".
kill_running_processes() {
    local binary_path="$1"
    local pattern
    pattern="^$(escape_ere "$binary_path")( |\$)"

    # pgrep/pkill match against the full command line; anchor to the exact path.
    if pgrep -f "$pattern" >/dev/null 2>&1; then
        print_info "Stopping running ${BINARY_NAME} processes..."
        pkill -f "$pattern" || true
        sleep 1

        # Force kill if still alive
        if pgrep -f "$pattern" >/dev/null 2>&1; then
            print_warn "Force killing ${BINARY_NAME} processes..."
            pkill -9 -f "$pattern" || true
            sleep 0.5
        fi

        # Final check — installation should not proceed if kill failed
        if pgrep -f "$pattern" >/dev/null 2>&1; then
            print_error "Could not stop ${BINARY_NAME} (PID: $(pgrep -f "$pattern" | tr '\n' ' '))"
            echo "  Kill manually then re-run: pkill -f '$pattern'"
            exit 1
        fi

        print_info "Stopped all ${BINARY_NAME} processes"
    fi
}

# 'claude mcp get' has no --scope flag; it searches all scopes. Existence here
# means "registered somewhere" — not necessarily in the user scope we manage.
mcp_server_exists() {
    if claude mcp get "$BINARY_NAME" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

configure_mcp_server() {
    local binary_path="$1"

    # Try to add at user scope unconditionally. 'claude mcp add' fails fast on
    # duplicate; we then verify existence to decide whether to treat that as
    # success (already-configured somewhere) or surface the real error.
    print_info "Registering MCP server with Claude Code (user scope)..."

    if claude mcp add --scope user "$BINARY_NAME" -- "$binary_path" >/dev/null 2>&1; then
        print_info "MCP server registered successfully"
        return 0
    fi

    if mcp_server_exists; then
        print_info "MCP server already registered (existing scope preserved)"
        return 0
    fi

    print_error "Failed to register Claude MCP server"
    echo "  Run manually: claude mcp add --scope user \"$BINARY_NAME\" -- \"$binary_path\""
    return 1
}

codex_mcp_server_exists() {
    if codex mcp get "$CODEX_MCP_NAME" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

# Returns:
#   0 - registered command equals "$binary_path"
#   1 - registered command differs
#   2 - could not inspect (codex failed, parser missing, malformed output)
#
# Distinguishing inspection failure from mismatch matters: install must not
# blindly remove+re-add when codex is broken, and uninstall must not silently
# leave a registration thinking "it's not ours" when we simply could not read.
codex_mcp_points_to_binary() {
    local binary_path="$1"
    local json configured

    if ! json=$(codex mcp get --json "$CODEX_MCP_NAME" 2>/dev/null); then
        return 2
    fi

    if command -v jq &>/dev/null; then
        configured=$(printf '%s' "$json" | jq -r '.transport.command // empty' 2>/dev/null) || return 2
    elif command -v python3 &>/dev/null; then
        configured=$(printf '%s' "$json" | python3 -c \
            'import sys, json; d = json.load(sys.stdin); print(d.get("transport", {}).get("command", "") or "")' \
            2>/dev/null) || return 2
    else
        # Neither jq nor python3 — refuse to guess from text output.
        return 2
    fi

    [[ -n "$configured" && "$configured" == "$binary_path" ]]
}

configure_codex_mcp_server() {
    local binary_path="$1"

    if codex_mcp_server_exists; then
        local match_rc=0
        codex_mcp_points_to_binary "$binary_path" || match_rc=$?
        case "$match_rc" in
            0)
                print_info "Codex MCP server configured: $CODEX_MCP_NAME"
                return 0
                ;;
            1)
                print_warn "Codex MCP server '$CODEX_MCP_NAME' points elsewhere; reconfiguring..."
                if ! codex mcp remove "$CODEX_MCP_NAME" >/dev/null 2>&1; then
                    print_error "Failed to remove stale Codex MCP server '$CODEX_MCP_NAME'"
                    echo "  Run manually: codex mcp remove \"$CODEX_MCP_NAME\" && codex mcp add \"$CODEX_MCP_NAME\" -- \"$binary_path\""
                    return 1
                fi
                ;;
            *)
                print_error "Could not inspect Codex MCP server '$CODEX_MCP_NAME'"
                echo "  Verify with: codex mcp get --json \"$CODEX_MCP_NAME\""
                echo "  Or install 'jq' / 'python3' so the installer can parse the registration."
                return 1
                ;;
        esac
    fi

    print_info "Registering MCP server with Codex CLI as '$CODEX_MCP_NAME'..."

    if codex mcp add "$CODEX_MCP_NAME" -- "$binary_path" >/dev/null 2>&1; then
        print_info "Codex MCP server registered successfully"
        return 0
    fi

    print_error "Failed to register Codex MCP server"
    echo "  Run manually: codex mcp add \"$CODEX_MCP_NAME\" -- \"$binary_path\""
    return 1
}

remove_mcp_server() {
    if ! mcp_server_exists; then
        print_info "MCP server not configured (user scope)"
        return 0
    fi

    print_info "Removing MCP server from Claude Code (user scope)..."
    if claude mcp remove --scope user "$BINARY_NAME" >/dev/null 2>&1; then
        print_info "MCP server removed"
    else
        print_error "Failed to remove MCP server"
        echo "  Run manually: claude mcp remove --scope user \"$BINARY_NAME\""
        return 1
    fi
}

remove_codex_mcp_server() {
    local binary_path="$1"

    if ! command -v codex &>/dev/null; then
        print_warn "Codex CLI not found — skipping Codex MCP removal"
        return 0
    fi

    if ! codex_mcp_server_exists; then
        print_info "Codex MCP server not configured"
        return 0
    fi

    local match_rc=0
    codex_mcp_points_to_binary "$binary_path" || match_rc=$?
    case "$match_rc" in
        0) ;;  # ours — proceed to remove
        1)
            print_warn "Codex MCP server '$CODEX_MCP_NAME' points elsewhere — leaving it configured"
            return 0
            ;;
        *)
            print_error "Could not inspect Codex MCP server '$CODEX_MCP_NAME' — leaving it configured"
            echo "  Verify with: codex mcp get --json \"$CODEX_MCP_NAME\""
            echo "  If it points to this binary, remove it manually: codex mcp remove \"$CODEX_MCP_NAME\""
            return 1
            ;;
    esac

    print_info "Removing MCP server from Codex CLI..."
    if codex mcp remove "$CODEX_MCP_NAME" >/dev/null 2>&1; then
        print_info "Codex MCP server removed"
    else
        print_error "Failed to remove Codex MCP server"
        echo "  Run manually: codex mcp remove \"$CODEX_MCP_NAME\""
        return 1
    fi
}

install_binary() {
    local install_dir="$1"
    local binary_src="$PROJECT_ROOT/target/release/$BINARY_NAME"

    if [ ! -f "$binary_src" ]; then
        print_error "Binary not found: $binary_src"
        echo "  Run 'make' first to build the release binary."
        exit 1
    fi

    print_info "Installing binary → $install_dir/$BINARY_NAME"
    cp "$binary_src" "$install_dir/$BINARY_NAME"
    chmod +x "$install_dir/$BINARY_NAME"
}

verify_installation() {
    local install_dir="$1"
    if [ ! -x "$install_dir/$BINARY_NAME" ]; then
        print_error "Binary installation failed or is not executable"
        exit 1
    fi
    print_info "Binary installed successfully"
}

show_binary_freshness() {
    local binary_path="$1"
    local release_path="$PROJECT_ROOT/target/release/$BINARY_NAME"

    if [ ! -f "$release_path" ]; then
        print_warn "Release binary not built: $release_path"
        return 0
    fi

    if find "$PROJECT_ROOT/src" "$PROJECT_ROOT/assets/ruleset.json" "$PROJECT_ROOT/Cargo.toml" "$PROJECT_ROOT/build.rs" -newer "$release_path" -print -quit 2>/dev/null | grep -q .; then
        print_warn "Source files are newer than target/release/$BINARY_NAME"
        echo "  Rebuild and reinstall with: make install"
    fi

    if cmp -s "$release_path" "$binary_path"; then
        print_info "Installed binary matches target/release/$BINARY_NAME"
    else
        print_warn "Installed binary differs from target/release/$BINARY_NAME"
        echo "  Reinstall with: make install"
    fi
}

# --- install ------------------------------------------------------------------

perform_install() {
    echo "=========================================="
    echo "  zhtw-mcp Installer"
    echo "=========================================="
    echo ""

    local install_dir
    install_dir=$(detect_install_dir)
    local binary_path="$install_dir/$BINARY_NAME"

    ensure_install_dir "$install_dir"

    # Must kill before overwriting — zhtw-mcp is a long-running MCP server.
    kill_running_processes "$binary_path"

    install_binary "$install_dir"
    verify_installation "$install_dir"
    check_path "$install_dir" || true

    local detected=0
    local failures=0
    if command -v claude &>/dev/null; then
        check_claude_cli >/dev/null
        configure_mcp_server "$binary_path" || failures=$((failures + 1))
        detected=1
    else
        print_warn "Claude CLI not found — skipping Claude MCP registration"
    fi

    if command -v codex &>/dev/null; then
        check_codex_cli >/dev/null
        configure_codex_mcp_server "$binary_path" || failures=$((failures + 1))
        detected=1
    else
        print_warn "Codex CLI not found — skipping Codex MCP registration"
    fi

    if [[ "$detected" -eq 0 ]]; then
        print_warn "No supported MCP client CLI found; binary installed only"
    fi

    echo ""
    echo "=========================================="
    if [[ "$failures" -gt 0 ]]; then
        echo "  Installation Complete (with $failures registration failure(s))"
    else
        echo "  Installation Complete"
    fi
    echo "=========================================="
    echo ""
    echo "Binary:  $binary_path"
    echo "MCP registration attempted for installed client CLIs"
    echo ""
    echo "Next step: Restart your MCP client so it launches the new binary"
    echo ""

    [[ "$failures" -eq 0 ]]
}

# --- uninstall ----------------------------------------------------------------

perform_uninstall() {
    echo "=========================================="
    echo "  zhtw-mcp Uninstaller"
    echo "=========================================="
    echo ""

    # Support non-interactive mode via ZHTW_YES=1 or --yes flag
    local auto_yes=0
    [[ "${ZHTW_YES:-0}" == "1" ]] && auto_yes=1
    [[ "${1:-}" == "--yes" ]] && auto_yes=1

    if [[ "$auto_yes" -eq 0 ]]; then
        if [ -t 0 ]; then
            read -r -p "Are you sure you want to uninstall $BINARY_NAME? [y/N] " -n 1 REPLY
            echo
            if [[ ! $REPLY =~ ^[Yy]$ ]]; then
                echo "Uninstallation cancelled"
                exit 0
            fi
        else
            print_error "Non-interactive terminal: use ZHTW_YES=1 or --yes to confirm uninstall"
            exit 1
        fi
    fi
    echo

    local install_dir
    install_dir=$(detect_install_dir)
    local binary_path="$install_dir/$BINARY_NAME"

    kill_running_processes "$binary_path"

    # Guard MCP registry cleanup so a CLI hiccup never strands the binary on
    # disk: set -e would otherwise abort before 'rm -f' below.
    local failures=0
    if command -v claude &>/dev/null; then
        remove_mcp_server || failures=$((failures + 1))
    else
        print_warn "Claude CLI not found — skipping Claude MCP removal"
    fi
    remove_codex_mcp_server "$binary_path" || failures=$((failures + 1))

    if [ -f "$binary_path" ]; then
        rm -f "$binary_path"
        print_info "Removed $binary_path"
    else
        print_warn "Binary not found at $binary_path"
    fi

    echo ""
    echo "=========================================="
    if [[ "$failures" -gt 0 ]]; then
        echo "  Uninstallation Complete (with $failures cleanup failure(s))"
    else
        echo "  Uninstallation Complete"
    fi
    echo "=========================================="
    echo ""
    echo "Binary removed from: $binary_path"
    echo "MCP server configuration removed where supported"
    echo ""

    [[ "$failures" -eq 0 ]]
}

# --- status -------------------------------------------------------------------

check_status() {
    local install_dir
    install_dir=$(detect_install_dir)
    local binary_path="$install_dir/$BINARY_NAME"

    print_status "Checking installation status..."
    echo ""

    if [ -x "$binary_path" ]; then
        print_info "Binary installed: $binary_path"
        show_binary_freshness "$binary_path"
    else
        print_warn "Binary not installed at $binary_path"
    fi

    # Use the exact installed path to avoid false positives from deploy.sh itself
    # or other processes whose argv contains "zhtw-mcp".
    local proc_pattern
    proc_pattern="^$(escape_ere "$binary_path")( |\$)"
    if pgrep -f "$proc_pattern" >/dev/null 2>&1; then
        print_info "Process is running (PID: $(pgrep -f "$proc_pattern" | tr '\n' ' '))"
    else
        print_info "Process is not running"
    fi

    check_path "$install_dir" || true

    if command -v claude &>/dev/null; then
        if mcp_server_exists; then
            print_info "Claude MCP server configured"
        else
            print_warn "Claude MCP server not configured"
        fi
    else
        print_warn "claude CLI not found — cannot check registration"
    fi

    if command -v codex &>/dev/null; then
        if codex_mcp_server_exists; then
            local status_match_rc=0
            codex_mcp_points_to_binary "$binary_path" || status_match_rc=$?
            case "$status_match_rc" in
                0)
                    print_info "Codex MCP server configured: $CODEX_MCP_NAME"
                    ;;
                1)
                    print_warn "Codex MCP server '$CODEX_MCP_NAME' is configured but points elsewhere"
                    echo "  Expected command: $binary_path"
                    ;;
                *)
                    print_warn "Could not inspect Codex MCP server '$CODEX_MCP_NAME'"
                    echo "  Verify with: codex mcp get --json \"$CODEX_MCP_NAME\""
                    ;;
            esac
        else
            print_warn "Codex MCP server '$CODEX_MCP_NAME' not configured"
        fi
    else
        print_warn "codex CLI not found — cannot check registration"
    fi
}

# --- dispatch -----------------------------------------------------------------

case "${1:-help}" in
    install)
        perform_install
        ;;
    uninstall)
        perform_uninstall "${2:-}"
        ;;
    status)
        check_status
        ;;
    help|"")
        echo "Usage: $0 [install|uninstall [--yes]|status]"
        echo ""
        echo "  install          Kill running server, install binary, register with detected MCP clients."
        echo "  uninstall        Kill server, remove binary, unregister from detected MCP clients."
        echo "  uninstall --yes  Non-interactive uninstall (also: ZHTW_YES=1)."
        echo "  status           Show binary, process, and registration state."
        ;;
    *)
        print_error "Unknown command: $1"
        exit 1
        ;;
esac
