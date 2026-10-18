#!/usr/bin/env python3
"""
Export US English (en-US) strings defined in tr! and tr_plural! macros in Rust code
by generating a main.ftl file that can be used for translating into other languages.

This script also creates a Psuedolocalized English (en-XA) main.ftl file with a given number of characters accented,
so that developers can easily detect which strings have been internationalized or not without needing to have
actual translations for a non-English language instead.
"""

import os
import re
import subprocess
import argparse
from pathlib import Path
from typing import Set, Dict, List, Tuple
import json
import collections
import hashlib

def find_rust_files(project_root: Path) -> List[Path]:
    """Find the project's Rust files, in a stable sorted order.

    Inside a git checkout this is every tracked or untracked-but-not-ignored
    `.rs` file, so ignored trees (target/, nested worktrees under .claude/)
    never contribute strings. Outside git it falls back to a directory walk.
    The order is sorted either way: a later file wins a key collision, so a
    filesystem-ordered walk would make two machines disagree.
    """
    try:
        out = subprocess.run(
            ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z', '--', '*.rs'],
            cwd=project_root, capture_output=True, check=True,
        ).stdout.decode('utf-8')
        rust_files = {project_root / f for f in out.split('\0') if f}
        return sorted(f for f in rust_files if f.is_file())
    except (OSError, subprocess.CalledProcessError):
        pass

    rust_files = []
    for root, dirs, files in os.walk(project_root):
        # Skip irrelevant directories
        dirs[:] = [d for d in dirs if d not in ['target', '.git', '.cargo']]

        for file in files:
            # Find only Rust source files
            if file.endswith('.rs'):
                rust_files.append(Path(root) / file)

    return sorted(rust_files)

def lex_rust(code: str) -> Tuple[str, str]:
    """Strip comments from Rust code, string-aware.

    Returns `(code, skeleton)`, two strings of equal length: `code` is the
    source with every comment removed (newlines kept, so line numbers hold),
    and `skeleton` is the same text with the inside of every string and char
    literal blanked to spaces. Searching and brace-matching on the skeleton
    can't be fooled by a `{` or `#[cfg(test)]` inside a string, and a `//`
    inside a string (a URL) is not mistaken for a comment.
    """
    out = []
    skel = []
    i = 0
    n = len(code)

    def emit(text: str, blank: bool = False):
        out.append(text)
        skel.append(re.sub(r'[^\n]', ' ', text) if blank else text)

    while i < n:
        c = code[i]
        nxt = code[i + 1] if i + 1 < n else ''
        if c == '/' and nxt == '/':
            end = code.find('\n', i)
            i = n if end == -1 else end
            continue
        if c == '/' and nxt == '*':
            depth = 1
            j = i + 2
            while j < n and depth:
                if code.startswith('/*', j):
                    depth += 1
                    j += 2
                elif code.startswith('*/', j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            emit('\n' * code.count('\n', i, j))
            i = j
            continue
        prev = code[i - 1] if i else ''
        raw = re.match(r'b?r(#*)"', code[i:]) if c in 'br' and not (prev.isalnum() or prev == '_') else None
        if raw:
            close = '"' + raw.group(1)
            start = i + raw.end()
            end = code.find(close, start)
            end = n if end == -1 else end
            emit(code[i:start])
            emit(code[start:end], blank=True)
            emit(code[end:end + len(close)])
            i = end + len(close)
            continue
        if c == '"':
            j = i + 1
            while j < n and code[j] != '"':
                j += 2 if code[j] == '\\' else 1
            emit('"')
            emit(code[i + 1:j], blank=True)
            emit('"')
            i = j + 1
            continue
        if c == "'":
            # A char literal ('{', '\'', '\u{7d}'), not a lifetime ('a).
            lit = re.match(r"'(\\(u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'])'", code[i:])
            if lit:
                emit("'")
                emit(lit.group(0)[1:-1], blank=True)
                emit("'")
                i += lit.end()
                continue
        emit(c)
        i += 1
    return ''.join(out), ''.join(skel)

def strip_rust_comments(code: str) -> str:
    """Remove line, block and doc comments from Rust code, leaving strings intact."""
    return lex_rust(code)[0]

def strip_test_code(code: str) -> str:
    """Remove comments and every `#[cfg(test)]` / `#[test]` item from Rust code.

    A string that only a test uses is not a string the app shows, and some
    tests assert the *absence* of an FTL entry (the i18n fallback tests), so
    exporting it would break them.
    """
    code, skel = lex_rust(code)
    cuts = []
    for m in re.finditer(r'#\[\s*(cfg\s*\(\s*test\s*\)|test)\s*\]', skel):
        if cuts and m.start() < cuts[-1][1]:
            continue  # inside an item already cut
        # The item ends at its first top-level `;` or its body's matching `}`.
        depth = 0
        end = None
        for j in range(m.end(), len(skel)):
            ch = skel[j]
            if ch == '{':
                depth += 1
            elif ch == '}':
                depth -= 1
                if depth == 0:
                    end = j + 1
                    break
            elif ch == ';' and depth == 0:
                end = j + 1
                break
        if end is not None:
            cuts.append((m.start(), end))
    for start, end in reversed(cuts):
        code = code[:start] + '\n' * code.count('\n', start, end) + code[end:]
    return code

def looks_like_path_or_url(message: str) -> bool:
    """True for a message that is a path, URL or email address, not UI text.

    Only these are skipped. A `/` alone is not enough: `{used} / {window}`
    and `{done}/{total} done` are UI strings, while `src/lib.rs`, `~/.config`
    and `https://damus.io` are not.
    """
    if re.search(r'[a-z][a-z0-9+.-]*://', message, re.IGNORECASE):
        return True
    if re.search(r'\bwww\.', message, re.IGNORECASE):
        return True
    if re.fullmatch(r'[^\s@]+@[^\s@]+\.[^\s@]+', message):
        return True
    if re.fullmatch(r'[\w.-]+\.(rs|ftl|toml|json|md)', message):
        return True
    for token in message.split():
        if '/' not in token and '\\' not in token.replace('\\"', ''):
            continue
        if re.match(r'(~|\.{1,2})?/[\w.-]', token):
            return True
        if re.match(r'(crates|src|target|assets)/', token):
            return True
        if re.search(r'\.[A-Za-z0-9]{1,5}$', token):
            return True
    return False

def extract_tr_macros_with_lines(content: str, file_path: str) -> dict:
    """Extract tr! macro calls from Rust code with comments and line numbers. Handles multi-line macros.

    `content` is code already passed through `strip_test_code`.
    """
    matches = []
    # Search the entire content for tr! macro calls (multi-line aware)
    for macro_content in extract_macro_calls(content, 'tr!'):
        args = parse_macro_arguments(macro_content)
        if len(args) >= 3:  # Must have at least message and comment
            message = args[1].strip()
            comment = args[2].strip()  # Second argument is always the comment
            # Validate placeholders
            if not validate_placeholders(message, file_path):
                continue
            if not looks_like_path_or_url(message):
                # Find the line number where this macro starts
                macro_start = f'tr!({macro_content}'
                idx = content.find(macro_start)
                line_num = content[:idx].count('\n') + 1 if idx != -1 else 1
                matches.append((message, comment, line_num, file_path))
    return matches

def extract_tr_plural_macros_with_lines(content: str, file_path: str) -> dict:
    """Extract tr_plural! macro calls from Rust code with new signature and correct keying, skipping macro definitions and doc comments."""
    matches = []
    # Skip macro definitions
    if 'macro_rules! tr_plural' in content or file_path.endswith('i18n/mod.rs'):
        return matches
    for idx, macro_content in enumerate(extract_macro_calls(content, 'tr_plural!')):
        args = parse_macro_arguments(macro_content)
        if len(args) >= 5:
            one = args[1].strip()
            other = args[2].strip()
            comment = args[3].strip()
            key = other
            if key and not key.startswith('$') and not looks_like_path_or_url(key):
                matches.append((key, comment, idx + 1, file_path))
    return matches

def parse_macro_arguments(content: str) -> List[str]:
    """Parse macro arguments, handling quoted strings, param = value pairs, commas, and inline comments."""
    # Collapse all whitespace/newlines to a single space
    content = re.sub(r'\s+', ' ', content.strip())
    args = []
    i = 0
    n = len(content)
    while i < n:
        # Skip whitespace
        while i < n and content[i].isspace():
            i += 1
        if i >= n:
            break
        # Handle quoted strings
        if content[i] in ['"', "'"]:
            quote_char = content[i]
            i += 1
            arg_start = i
            while i < n:
                if content[i] == '\\' and i + 1 < n:
                    i += 2
                elif content[i] == quote_char:
                    break
                else:
                    i += 1
            arg = content[arg_start:i]
            args.append(arg)
            i += 1  # Skip closing quote
        else:
            arg_start = i
            paren_count = 0
            brace_count = 0
            while i < n:
                char = content[i]
                if char == '(':
                    paren_count += 1
                elif char == ')':
                    paren_count -= 1
                elif char == '{':
                    brace_count += 1
                elif char == '}':
                    brace_count -= 1
                elif char == ',' and paren_count == 0 and brace_count == 0:
                    break
                i += 1
            arg = content[arg_start:i].strip()
            if arg:
                args.append(arg)
        # Skip the comma if we found one
        if i < n and content[i] == ',':
            i += 1
    return args

def extract_macro_calls(content: str, macro_name: str):
    """Extract all macro calls of the given macro_name from the entire content, handling parentheses inside quoted strings and multi-line macros."""
    calls = []
    idx = 0
    macro_start = f'{macro_name}('
    content_len = len(content)
    while idx < content_len:
        start = content.find(macro_start, idx)
        if start == -1:
            break
        prev = content[start - 1] if start else ''
        if prev.isalnum() or prev == '_':
            # `include_str!(` is not `tr!(`
            idx = start + len(macro_start)
            continue
        i = start + len(macro_start)
        paren_count = 1  # Start after the initial '('
        in_quote = False
        quote_char = ''
        macro_content = ''
        while i < content_len:
            c = content[i]
            if in_quote:
                macro_content += c
                if c == quote_char and (i == 0 or content[i-1] != '\\'):
                    in_quote = False
            else:
                if c in ('"', "'"):
                    in_quote = True
                    quote_char = c
                    macro_content += c
                elif c == '(':
                    paren_count += 1
                    macro_content += c
                elif c == ')':
                    paren_count -= 1
                    if paren_count == 0:
                        break
                    else:
                        macro_content += c
                else:
                    macro_content += c
            i += 1
        # Only add if we found a closing parenthesis
        if i < content_len and content[i] == ')':
            calls.append(macro_content)
            idx = i + 1
        else:
            # Malformed macro, skip past this occurrence
            idx = start + len(macro_start)
    return calls

def validate_placeholders(message: str, file_path: str = "") -> bool:
    """Validate that all placeholders in a message are named and start with a letter."""
    import re

    # Find all placeholders in the message
    placeholder_pattern = r'\{([^}]*)\}'
    placeholders = re.findall(placeholder_pattern, message)

    valid = True
    for placeholder in placeholders:
        if not placeholder.strip():
            print(f"[VALIDATE] Warning: Empty placeholder {{}} found in message: '{message[:100]}...' {file_path}")
            valid = False
        elif not placeholder[0].isalpha():
            print(f"[VALIDATE] Warning: Placeholder '{{{placeholder}}}' does not start with a letter in message: '{message[:100]}...' {file_path}")
            valid = False
    if not valid:
        print(f"[VALIDATE] Message rejected: '{message}'")
    return valid

def extract_tr_macros(content: str, verbose: bool = False) -> List[Tuple[str, str]]:
    """Extract tr! macro calls from Rust code with comments.

    `content` is code already passed through `strip_test_code`.
    """
    filtered_matches = []
    # Process the entire content instead of line by line to handle multi-line macros
    for macro_content in extract_macro_calls(content, 'tr!'):
        args = parse_macro_arguments(macro_content)
        if len(args) >= 3:  # Must have at least message and comment
            message = args[1].strip()
            comment = args[2].strip()  # Second argument is always the comment
            # Validate placeholders
            if not validate_placeholders(message):
                continue
            if not looks_like_path_or_url(message):
                # Store as (message, comment) tuple to preserve all combinations
                filtered_matches.append((message, comment))
    return filtered_matches

def extract_tr_plural_macros(content: str, file_path: str = "") -> Dict[str, dict]:
    """Extract tr_plural! macro calls from Rust code with new signature, skipping macro definitions.

    `content` is code already passed through `strip_test_code`. The result is
    keyed by the FTL key (the `other` message plus a hash of its comment), so
    two calls with the same text and different comments both survive.
    """
    filtered_matches = {}
    # Skip macro definitions
    if 'macro_rules! tr_plural' in content or file_path.endswith('i18n/mod.rs'):
        return filtered_matches
    for macro_content in extract_macro_calls(content, 'tr_plural!'):
        args = parse_macro_arguments(macro_content)
        if len(args) >= 5:
            one = args[1].strip()
            other = args[2].strip()
            comment = args[3].strip()
            key = other
            if key and not key.startswith('$') and not looks_like_path_or_url(key):
                filtered_matches[normalize_key(key, comment)] = {
                    'one': one,
                    'other': other,
                    'comment': comment
                }
    return filtered_matches

def escape_rust_placeholders(text: str) -> str:
    """Convert Rust-style placeholders to Fluent-style placeholders"""
    # Unescape double quotes first
    text = text.replace('\\"', '"')
    # Convert Rust placeholders to Fluent placeholders
    return re.sub(r'\{([a-zA-Z][a-zA-Z0-9_]*)\}', r'{$\1}', text)

def simple_hash(s: str) -> str:
    """Simple hash function using MD5 - matches Rust implementation, 4 hex chars"""
    return hashlib.md5(s.encode('utf-8')).hexdigest()[:4]

def normalize_key(message, comment=None):
    """Normalize a message to create a consistent key - matches Rust normalize_ftl_key function"""
    # Remove quotes and normalize
    key = message.strip('"\'')
    # Unescape double quotes
    key = key.replace('\\"', '"')
    # Replace each invalid character with exactly one underscore (allow hyphens and underscores)
    key = re.sub(r'[^a-zA-Z0-9_-]', '_', key)
    # Remove leading/trailing underscores
    key = key.strip('_')
    # Add 'k_' prefix if the result doesn't start with a letter (Fluent requirement)
    if not (key and key[0].isalpha()):
        key = "k_" + key

    # If we have a comment, append a hash of it to reduce collisions
    if comment:
        # Create a hash of the comment and append it to the key
        hash_str = f"_{simple_hash(comment)}"
        key += hash_str

    return key

def pseudolocalize(text: str) -> str:
    """Convert English text to pseudolocalized text for testing."""
    # Common pseudolocalization patterns
    replacements = {
        'a': 'à', 'e': 'é', 'i': 'í', 'o': 'ó', 'u': 'ú',
        'A': 'À', 'E': 'É', 'I': 'Í', 'O': 'Ó', 'U': 'Ú',
        'n': 'ñ', 'N': 'Ñ', 'c': 'ç', 'C': 'Ç'
    }

    # First, protect Fluent placeables from pseudolocalization
    placeable_pattern = r'\{ *\$[a-zA-Z][a-zA-Z0-9_]* *\}'
    placeables = re.findall(placeable_pattern, text)

    # Replace placeables with unique placeholders that won't be modified
    protected_text = text
    for i, placeable in enumerate(placeables):
        placeholder = f"<<PLACEABLE_{i}>>"
        protected_text = protected_text.replace(placeable, placeholder, 1)

    # Apply character replacements, skipping <<PLACEABLE_n>>
    result = ''
    i = 0
    while i < len(protected_text):
        if protected_text.startswith('<<PLACEABLE_', i):
            end = protected_text.find('>>', i)
            if end != -1:
                result += protected_text[i:end+2]
                i = end + 2
                continue
        char = protected_text[i]
        result += replacements.get(char, char)
        i += 1

    # Restore placeables
    for i, placeable in enumerate(placeables):
        placeholder = f"<<PLACEABLE_{i}>>"
        result = result.replace(placeholder, placeable)

    # Wrap pseudolocalized string with square brackets so that it can be distinguished from other strings
    return f'{{"["}}{result}{{"]"}}'

def generate_ftl_content(tr_strings: Dict[str, str],
                        plural_strings: Dict[str, dict],
                        tr_occurrences: Dict[Tuple[str, str], list],
                        plural_occurrences: Dict[Tuple[str, str], list],
                        pseudolocalize_content: bool = False) -> str:
    """Generate FTL file content from extracted strings with comments."""

    lines = [
        "# Main translation file for Notedeck",
        "# This file contains common UI strings used throughout the application",
        "# Auto-generated by extract_i18n.py - DO NOT EDIT MANUALLY",
        "",
    ]

    # Sort strings for consistent output. The exact key breaks a tie between
    # keys that differ only in case, so the order never depends on which file
    # was read first.
    sorted_tr = sorted(tr_strings.items(), key=lambda item: (item[0].lower(), item[0]))
    sorted_plural = sorted(plural_strings.items(),
                           key=lambda item: (item[1]['other'].lower(), item[0]))

    # Add regular tr! strings
    if sorted_tr:
        lines.append("# Regular strings")
        for norm_key, (original_message, comment) in sorted_tr:
            lines.append("")
            # Write the comment
            if comment:
                lines.append(f"# {comment}")
            # Apply pseudolocalization if requested
            value = escape_rust_placeholders(original_message)
            value = pseudolocalize(value) if pseudolocalize_content else value
            lines.append(f"{norm_key} = {value}")
        lines.append("")

    # Add pluralized strings
    if sorted_plural:
        lines.append("# Pluralized strings")
        for norm_key, data in sorted_plural:
            lines.append("")

            one = data['one']
            other = data['other']
            comment = data['comment']
            # Write comment
            if comment:
                lines.append(f"# {comment}")
            one_val = escape_rust_placeholders(one)
            other_val = escape_rust_placeholders(other)
            if pseudolocalize_content:
                one_val = pseudolocalize(one_val)
                other_val = pseudolocalize(other_val)
            lines.append(f'{norm_key} =')
            lines.append(f'    {{ $count ->')
            lines.append(f'        [one] {one_val}')
            lines.append(f'       *[other] {other_val}')
            lines.append(f'    }}')
            lines.append("")

    return "\n".join(lines)

def read_existing_ftl(ftl_path: Path) -> Dict[str, str]:
    """Read existing FTL file to preserve comments and custom translations."""
    if not ftl_path.exists():
        return {}

    existing_translations = {}
    with open(ftl_path, 'r', encoding='utf-8') as f:
        content = f.read()

    # Extract key-value pairs
    pattern = r'^([^#\s][^=]*?)\s*=\s*(.+)$'
    for line in content.split('\n'):
        match = re.match(pattern, line.strip())
        if match:
            key = match.group(1).strip()
            value = match.group(2).strip()
            # For existing FTL files, we need to handle keys that may have hash suffixes
            # Strip the hash suffix if present (8 hex characters after underscore)
            original_key = re.sub(r'_[0-9a-f]{8}$', '', key)
            norm_key = normalize_key(original_key)
            existing_translations[norm_key] = value

    return existing_translations

def ftl_entries(content: str) -> Dict[str, str]:
    """Map each message key in FTL `content` to its whole entry text (comment, value, variants)."""
    entries = {}
    key = None
    lines = []
    for line in content.split('\n'):
        match = re.match(r'^([A-Za-z][A-Za-z0-9_-]*) =', line)
        if match:
            key = match.group(1)
            lines = [line]
            entries[key] = lines
        elif key and line.startswith(' '):
            lines.append(line)
        else:
            key = None
    return {k: '\n'.join(v) for k, v in entries.items()}

def report_ftl_drift(path: Path, on_disk: str, generated: str):
    """Print the keys that regenerating `path` would add, remove or change."""
    old = ftl_entries(on_disk)
    new = ftl_entries(generated)
    added = sorted(new.keys() - old.keys())
    removed = sorted(old.keys() - new.keys())
    changed = sorted(k for k in old.keys() & new.keys() if old[k] != new[k])
    print(f"\n{path} is out of date:")
    for label, keys in (('added', added), ('removed', removed), ('changed', changed)):
        for k in keys:
            print(f"  {label}: {k}")
    if not (added or removed or changed):
        print("  same keys, different order or comments")

def main():
    parser = argparse.ArgumentParser(description='Extract i18n macros and generate FTL file')
    parser.add_argument('--project-root', type=str, default='.',
                       help='Project root directory (default: current directory)')
    parser.add_argument('--dry-run', action='store_true',
                       help='Show what would be generated without writing to file')
    parser.add_argument('--fail-on-collisions', action='store_true',
                       help='Exit with error if key collisions are detected')
    parser.add_argument('--check', action='store_true',
                       help='Write nothing; exit 1 and name the keys that differ if the '
                            'FTL files on disk are out of date with the source')
    parser.add_argument('-v', '--verbose', action='store_true',
                       help='Print per-file counts and every extracted key')

    args = parser.parse_args()

    project_root = Path(args.project_root)

    print(f"Scanning Rust files in {project_root}...")

    # Find all Rust files
    rust_files = find_rust_files(project_root)
    print(f"Found {len(rust_files)} Rust files")

    # Extract strings from all files
    all_tr_strings = {}
    all_plural_strings = {}

    # Track normalized keys to detect actual key collisions
    all_tr_normalized_keys = {}
    all_plural_normalized_keys = {}

    # Track collisions
    tr_collisions = {}
    plural_collisions = {}

    # Track all occurrences for intra-file collision detection
    tr_occurrences = collections.defaultdict(list)
    plural_occurrences = collections.defaultdict(list)

    for rust_file in rust_files:
        try:
            with open(rust_file, 'r', encoding='utf-8') as f:
                content = f.read()

            code = strip_test_code(content)

            # For intra-file collision detection
            tr_lines = extract_tr_macros_with_lines(code, str(rust_file))
            for key, comment, line, file_path in tr_lines:
                tr_occurrences[(file_path, key)].append((comment, line))
            plural_lines = extract_tr_plural_macros_with_lines(code, str(rust_file))
            for key, comment, line, file_path in plural_lines:
                plural_occurrences[(file_path, key)].append((comment, line))

            tr_strings = extract_tr_macros(code)
            plural_strings = extract_tr_plural_macros(code, str(rust_file))

            if (tr_strings or plural_strings) and args.verbose:
                print(f"  {rust_file}: {len(tr_strings)} tr!, {len(plural_strings)} tr_plural!")

            # Check for collisions in tr! strings using normalized keys
            for message, comment in tr_strings:
                norm_key = normalize_key(message, comment)
                if norm_key in all_tr_normalized_keys and all_tr_strings[norm_key] != (message, comment):
                    # A real key collision: two different messages normalize to one key.
                    # The same message and comment used twice is just reuse.
                    if norm_key not in tr_collisions:
                        tr_collisions[norm_key] = []
                    tr_collisions[norm_key].append((rust_file, all_tr_normalized_keys[norm_key]))
                    tr_collisions[norm_key].append((rust_file, comment))
                # Store by normalized key to preserve all unique combinations
                all_tr_strings[norm_key] = (message, comment)
                all_tr_normalized_keys[norm_key] = comment

            # Check for collisions in plural strings using normalized keys
            for norm_key, data in plural_strings.items():
                if norm_key in all_plural_normalized_keys and all_plural_strings[norm_key] != data:
                    # A real key collision: two different messages normalize to one key.
                    if norm_key not in plural_collisions:
                        plural_collisions[norm_key] = []
                    plural_collisions[norm_key].append((rust_file, all_plural_normalized_keys[norm_key]))
                    plural_collisions[norm_key].append((rust_file, data))
                all_plural_strings[norm_key] = data
                all_plural_normalized_keys[norm_key] = data

        except Exception as e:
            print(f"Error reading {rust_file}: {e}")

    # Intra-file collision detection. One message with two comments gets two
    # keys, so this is only worth printing when asked for.
    has_intra_file_collisions = False
    for (file_path, key), occurrences in tr_occurrences.items():
        comments = set(c for c, _ in occurrences)
        if len(occurrences) > 1 and len(comments) > 1:
            has_intra_file_collisions = True
            if not (args.verbose or args.fail_on_collisions):
                continue
            print(f"\n⚠️  Intra-file key collision in {file_path} for '{key}':")
            for comment, line in occurrences:
                comment_text = f" (comment: '{comment}')" if comment else " (no comment)"
                print(f"    Line {line}{comment_text}")
    for (file_path, key), occurrences in plural_occurrences.items():
        comments = set(c for c, _ in occurrences)
        if len(occurrences) > 1 and len(comments) > 1:
            has_intra_file_collisions = True
            if not (args.verbose or args.fail_on_collisions):
                continue
            print(f"\n⚠️  Intra-file key collision in {file_path} for '{key}':")
            for comment, line in occurrences:
                comment_text = f" (comment: '{comment}')" if comment else " (no comment)"
                print(f"    Line {line}{comment_text}")
    if has_intra_file_collisions and args.fail_on_collisions:
        print(f"❌ Exiting due to intra-file key collisions (--fail-on-collisions flag)")
        exit(1)

    # Report collisions
    has_collisions = False

    if tr_collisions:
        has_collisions = True
        print(f"\n⚠️  Key collisions detected in tr! strings:")
        for key, collisions in tr_collisions.items():
            print(f"  '{key}':")
            for file_path, comment in collisions:
                comment_text = f" (comment: '{comment}')" if comment else " (no comment)"
                print(f"    {file_path}{comment_text}")

    if plural_collisions:
        has_collisions = True
        print(f"\n⚠️  Key collisions detected in tr_plural! strings:")
        for key, collisions in plural_collisions.items():
            print(f"  '{key}':")
            for file_path, comment in collisions:
                comment_text = f" (comment: '{comment}')" if comment else " (no comment)"
                print(f"    {file_path}{comment_text}")

    if has_collisions:
        print(f"\n💡 Collision resolution: The last occurrence of each key will be used.")
        if args.fail_on_collisions:
            print(f"❌ Exiting due to key collisions (--fail-on-collisions flag)")
            exit(1)

    print(f"\nExtracted strings:")
    print(f"  Regular strings: {len(all_tr_strings)}")
    print(f"  Plural strings: {len(all_plural_strings)}")

    if args.verbose:
        print("All tr! keys:")
        for k in sorted(all_tr_strings.keys()):
            print(f"  {k}")

    # Generate FTL content for both locales
    locales = ['en-US', 'en-XA']

    stale = []
    for locale in locales:
        pseudolocalize_content = (locale == 'en-XA')
        ftl_content = generate_ftl_content(all_tr_strings, all_plural_strings, tr_occurrences, plural_occurrences, pseudolocalize_content)
        output_path = project_root / 'assets' / 'translations' / locale / 'main.ftl'

        if args.check:
            on_disk = output_path.read_text(encoding='utf-8') if output_path.exists() else ''
            if on_disk != ftl_content:
                stale.append(output_path)
                report_ftl_drift(output_path, on_disk, ftl_content)
        elif args.dry_run:
            print(f"\n--- Generated FTL content for {locale} ---")
            print(ftl_content)
            print(f"--- End of content for {locale} ---")
        else:
            # Ensure output directory exists
            output_path.parent.mkdir(parents=True, exist_ok=True)

            # Write to file
            with open(output_path, 'w', encoding='utf-8') as f:
                f.write(ftl_content)

            print(f"\nGenerated FTL file: {output_path}")

    if args.check:
        if stale:
            print("\n❌ FTL files are out of date. Run: python3 scripts/export_source_strings.py")
            exit(1)
        print("\n✅ FTL files are up to date")
    elif not args.dry_run:
        print(f"\nTotal strings: {len(all_tr_strings) + len(all_plural_strings)}")

if __name__ == '__main__':
    main()