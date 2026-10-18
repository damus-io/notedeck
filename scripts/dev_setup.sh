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
chmod +x "$GIT_HOOKS_DIR/pre-commit"

# Copy the commit-msg hook script
cp -p "$HOOK_SCRIPTS_DIR/commit_msg_hook.sh" "$GIT_HOOKS_DIR/commit-msg"
chmod +x "$GIT_HOOKS_DIR/commit-msg"

# The pre-push hook uploads Git LFS objects (snapshot pngs). Don't clobber a
# pre-push hook of your own: chain ours into it instead.
if [ -e "$GIT_HOOKS_DIR/pre-push" ] && ! cmp -s "$HOOK_SCRIPTS_DIR/pre_push_hook.sh" "$GIT_HOOKS_DIR/pre-push"; then
  echo "Note: $GIT_HOOKS_DIR/pre-push already exists, so it was left alone."
  echo "  Add this line to it so pushes upload Git LFS objects: git lfs pre-push \"\$@\""
else
  cp -p "$HOOK_SCRIPTS_DIR/pre_push_hook.sh" "$GIT_HOOKS_DIR/pre-push"
  chmod +x "$GIT_HOOKS_DIR/pre-push"
fi

echo "Git hooks have been set up successfully."
