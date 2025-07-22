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
import argparse
from pathlib import Path
from typing import Set, Dict, List, Tuple
import json
import collections
import hashlib

def find_rust_files(project_root: Path) -> List[Path]:
    """Find all Rust files in the project."""
    rust_files = []
    for root, dirs, files in os.walk(project_root):
        # Skip irrelevant directories
        dirs[:] = [d for d in dirs if d not in ['target', '.git', '.cargo']]

        for file in files:
            # Find only Rust source files
            if file.endswith('.rs'):
                rust_files.append(Path(root) / file)

    return rust_files

def strip_rust_comments(code: str) -> str:
    """Remove // line comments, /* ... */ block comments, and doc comments (///, //!, //! ...) from Rust code."""
    # Remove block comments first
    code = re.sub(r'/\*.*?\*/', '', code, flags=re.DOTALL)
    # Remove line comments
    code = re.sub(r'//.*', '', code)
    # Remove doc comments (/// and //! at start of line)
    code = re.sub(r'^\s*///.*$', '', code, flags=re.MULTILINE)
    code = re.sub(r'^\s*//!.*$', '', code, flags=re.MULTILINE)
    return code

def extract_tr_macros_with_lines(content: str, file_path: str) -> dict:
    """Extract tr! macro calls from Rust code with comments and line numbers. Handles multi-line macros."""
    matches = []
    # Strip comments before processing
    content = strip_rust_comments(content)
    # Search the entire content for tr! macro calls (multi-line aware)
    for macro_content in extract_macro_calls(content, 'tr!'):
        args = parse_macro_arguments(macro_content)
        if len(args) >= 3:  # Must have at least message and comment
            message = args[1].strip()
            comment = args[2].strip()  # Second argument is always the comment
            # Validate placeholders
            if not validate_placeholders(message, file_path):
                continue
            if not any(skip in message.lower() for skip in [
                '/', '\\', '.ftl', '.rs', 'http', 'https', 'www', '@',
                'crates/', 'src/', 'target/', 'build.rs']):
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
            if key and not key.startswith('//') and not key.startswith('$'):
                matches.append((key, comment, idx + 1, file_path))
    return matches

def parse_macro_arguments(content: str) -> List[str]:
    """Parse macro arguments, handling quoted strings, param = value pairs, commas, and inline comments."""
    # Remove all // comments
    content = re.sub(r'//.*', '', content)
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

def extract_tr_macros(content: str) -> List[Tuple[str, str]]:
    """Extract tr! macro calls from Rust code with comments."""
    filtered_matches = []
    # Strip comments before processing
    content = strip_rust_comments(content)
    # Process the entire content instead of line by line to handle multi-line macros
    for macro_content in extract_macro_calls(content, 'tr!'):
        args = parse_macro_arguments(macro_content)
        if len(args) >= 3:  # Must have at least message and comment
            message = args[1].strip()
            comment = args[2].strip()  # Second argument is always the comment
            # Debug output for identification strings
            if "identification" in comment.lower():
                print(f"[DEBUG] Found identification tr! macro: message='{message}', comment='{comment}', args={args}")
                norm_key = normalize_key(message, comment)
                print(f"[DEBUG] Normalized key: '{norm_key}'")
            # Validate placeholders
            if not validate_placeholders(message):
                continue
            # More specific filtering logic
            should_skip = False
            for skip in ['/', '.ftl', '.rs', 'http', 'https', 'www', 'crates/', 'src/', 'target/', 'build.rs']:
                if skip in message.lower():
                    should_skip = True
                    break
            # Special handling for @ - only skip if it looks like an actual email address
            if '@' in message and (
                # Skip if it's a short string that looks like an email
                len(message) < 50 or
                # Skip if it contains common email patterns
                any(pattern in message.lower() for pattern in ['@gmail.com', '@yahoo.com', '@hotmail.com', '@outlook.com'])
            ):
                should_skip = True
            if not should_skip:
                # Store as (message, comment) tuple to preserve all combinations
                filtered_matches.append((message, comment))
    return filtered_matches

def extract_tr_plural_macros(content: str, file_path: str = "") -> Dict[str, dict]:
    """Extract tr_plural! macro calls from Rust code with new signature, skipping macro definitions and doc comments."""
    filtered_matches = {}
    # Skip macro definitions
    if 'macro_rules! tr_plural' in content or file_path.endswith('i18n/mod.rs'):
        print(f"[DEBUG] Skipping macro definitions in {file_path}")
        return filtered_matches
    for macro_content in extract_macro_calls(content, 'tr_plural!'):
        print(f"[DEBUG] Found tr_plural! macro in {file_path}: {macro_content}")
        args = parse_macro_arguments(macro_content)
        print(f"[DEBUG] Parsed args: {args}")
        if len(args) >= 5:
            one = args[1].strip()
            other = args[2].strip()
            comment = args[3].strip()
            key = other
            if key and not key.startswith('//') and not key.startswith('$'):
                print(f"[DEBUG] Adding plural key '{key}' from {file_path}")
                filtered_matches[key] = {
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

    # Sort strings for consistent output
    sorted_tr = sorted(tr_strings.items(), key=lambda item: item[0].lower())
    sorted_plural = sorted(plural_strings.items(), key=lambda item: item[0].lower())

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
        for key, data in sorted_plural:
            lines.append("")

            one = data['one']
            other = data['other']
            comment = data['comment']
            # Write comment
            if comment:
                lines.append(f"# {comment}")
            norm_key = normalize_key(key, comment)
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

def main():
    parser = argparse.ArgumentParser(description='Extract i18n macros and generate FTL file')
    parser.add_argument('--project-root', type=str, default='.',
                       help='Project root directory (default: current directory)')
    parser.add_argument('--dry-run', action='store_true',
                       help='Show what would be generated without writing to file')
    parser.add_argument('--fail-on-collisions', action='store_true',
                       help='Exit with error if key collisions are detected')

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

            # For intra-file collision detection
            tr_lines = extract_tr_macros_with_lines(content, str(rust_file))
            for key, comment, line, file_path in tr_lines:
                tr_occurrences[(file_path, key)].append((comment, line))
            plural_lines = extract_tr_plural_macros_with_lines(content, str(rust_file))
            for key, comment, line, file_path in plural_lines:
                plural_occurrences[(file_path, key)].append((comment, line))

            tr_strings = extract_tr_macros(content)
            plural_strings = extract_tr_plural_macros(content, str(rust_file))

            if tr_strings or plural_strings:
                print(f"  {rust_file}: {len(tr_strings)} tr!, {len(plural_strings)} tr_plural!")

            # Check for collisions in tr! strings using normalized keys
            for message, comment in tr_strings:
                norm_key = normalize_key(message, comment)
                if norm_key in all_tr_normalized_keys:
                    # This is a real key collision (same normalized key)
                    if norm_key not in tr_collisions:
                        tr_collisions[norm_key] = []
                    tr_collisions[norm_key].append((rust_file, all_tr_normalized_keys[norm_key]))
                    tr_collisions[norm_key].append((rust_file, comment))
                # Store by normalized key to preserve all unique combinations
                all_tr_strings[norm_key] = (message, comment)
                all_tr_normalized_keys[norm_key] = comment

            # Check for collisions in plural strings using normalized keys
            for key, data in plural_strings.items():
                comment = data['comment']
                norm_key = normalize_key(key, comment)
                if norm_key in all_plural_normalized_keys:
                    # This is a real key collision (same normalized key)
                    if norm_key not in plural_collisions:
                        plural_collisions[norm_key] = []
                    plural_collisions[norm_key].append((rust_file, all_plural_normalized_keys[norm_key]))
                    plural_collisions[norm_key].append((rust_file, data))
                all_plural_strings[key] = data
                all_plural_normalized_keys[norm_key] = data

        except Exception as e:
            print(f"Error reading {rust_file}: {e}")

    # Intra-file collision detection
    has_intra_file_collisions = False
    for (file_path, key), occurrences in tr_occurrences.items():
        comments = set(c for c, _ in occurrences)
        if len(occurrences) > 1 and len(comments) > 1:
            has_intra_file_collisions = True
            print(f"\n⚠️  Intra-file key collision in {file_path} for '{key}':")
            for comment, line in occurrences:
                comment_text = f" (comment: '{comment}')" if comment else " (no comment)"
                print(f"    Line {line}{comment_text}")
    for (file_path, key), occurrences in plural_occurrences.items():
        comments = set(c for c, _ in occurrences)
        if len(occurrences) > 1 and len(comments) > 1:
            has_intra_file_collisions = True
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

    # Debug: print all keys in all_tr_strings
    print("[DEBUG] All tr! keys:")
    for k in all_tr_strings.keys():
        print(f"  {k}")

    # Generate FTL content for both locales
    locales = ['en-US', 'en-XA']

    for locale in locales:
        pseudolocalize_content = (locale == 'en-XA')
        ftl_content = generate_ftl_content(all_tr_strings, all_plural_strings, tr_occurrences, plural_occurrences, pseudolocalize_content)
        output_path = Path(f'assets/translations/{locale}/main.ftl')

        if args.dry_run:
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

    if not args.dry_run:
        print(f"\nTotal strings: {len(all_tr_strings) + len(all_plural_strings)}")

if __name__ == '__main__':
    main()