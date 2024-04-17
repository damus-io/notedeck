#!/usr/bin/env bash

HOOK_SCRIPTS_DIR="scripts"
GIT_HOOKS_DIR=".git/hooks"

# Ensure the necessary directories exist and are accessible
if [ ! -d "$HOOK_SCRIPTS_DIR" ] || [ ! -d "$GIT_HOOKS_DIR" ]; then
  echo "Error: Required directories are missing. Please ensure you are in the project's root directory."
  exit 1
fi

# Copy the pre-commit hook script
cp -p "$HOOK_SCRIPTS_DIR/pre_commit_hook.sh" "$GIT_HOOKS_DIR/pre-commit"

# Make the hook script executable
chmod +x "$GIT_HOOKS_DIR/pre-commit"

echo "Pre-commit hook has been set up successfully."

