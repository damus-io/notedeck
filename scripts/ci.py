#!/usr/bin/env python3
"""
Local CI runner — parses .github/workflows/rust.yml and runs jobs locally.

The GitHub workflow YAML is the single source of truth. This script reads it,
resolves reusable workflows, skips GitHub-specific actions (checkout, upload,
setup-java, etc.), and executes the `run:` steps directly.

Usage:
    ./scripts/ci.py                  # Run default jobs (lint + test)
    ./scripts/ci.py lint             # Run a specific job
    ./scripts/ci.py lint linux-test  # Run multiple jobs
    ./scripts/ci.py list             # List all jobs
    ./scripts/ci.py --all            # Run all jobs viable on this platform
    ./scripts/ci.py --dry-run lint   # Show what would run

Options:
    --base BRANCH   Base branch for changelog check (default: master)
    --dry-run       Print commands without executing
    -v / --verbose  Show command output even on success
    --all           Run all jobs viable on current platform
    --workflow FILE  Path to workflow YAML (default: .github/workflows/rust.yml)
    -j / --jobs N   Max parallel jobs (default: half of CPU cores)
"""

import argparse
import multiprocessing
import os
import platform
import re
import subprocess
import sys
import time
from pathlib import Path

try:
    import yaml
except ImportError:
    print("PyYAML required. Install with: pip install pyyaml")
    sys.exit(1)


# --- Colors ---

def _supports_color():
    return hasattr(sys.stdout, "isatty") and sys.stdout.isatty()

COLORS = _supports_color()

def color(code, text):
    return f"\033[{code}m{text}\033[0m" if COLORS else text

def bold(t):   return color("1", t)
def green(t):  return color("32", t)
def red(t):    return color("31", t)
def yellow(t): return color("33", t)
def dim(t):    return color("2", t)


# --- Helpers ---

def repo_root():
    res = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True
    )
    return res.stdout.strip()


def native_arch():
    m = platform.machine()
    return {"arm64": "aarch64", "AMD64": "x86_64"}.get(m, m)


def current_os():
    """Map platform to GitHub runner OS prefix."""
    return {"Linux": "linux", "Darwin": "macos", "Windows": "windows"}.get(
        platform.system(), "unknown"
    )


def runs_on_matches(runs_on, current):
    """Check if a runs-on value is compatible with current platform."""
    if isinstance(runs_on, str):
        val = runs_on.lower()
        # Handle expressions like ${{ inputs.os }}
        if "${{" in val:
            return True  # Can't resolve, assume yes
        if current == "linux" and "ubuntu" in val:
            return True
        if current == "macos" and "macos" in val:
            return True
        if current == "windows" and "windows" in val:
            return True
    return False


# GitHub Actions that we skip (they set up the environment, not run code)
SKIP_ACTIONS = {
    "actions/checkout",
    "actions/upload-artifact",
    "actions/download-artifact",
    "dtolnay/rust-toolchain",
    "android-actions/setup-android",
    "actions/setup-java",
    "apple-actions/import-codesign-certs",
    "Swatinem/rust-cache",
}


def is_install_step(step):
    """Return True if this step installs packages/tools (CI setup, not needed locally)."""
    script = step.get("run", "")
    patterns = [
        "apt-get install",
        "apt install",
        "brew install",
        "cargo install",
        "rustup target add",
        "choco install",
        "pip install",
    ]
    return any(p in script for p in patterns)


def should_skip_step(step):
    """Return True if this step is a GitHub-only action we can't run locally."""
    if "uses" in step:
        action = step["uses"].split("@")[0]
        return action in SKIP_ACTIONS
    if is_install_step(step):
        return True
    return False


def expand_matrix_vars(text, matrix_values):
    """Replace ${{ matrix.X }} with the provided values."""
    if not isinstance(text, str):
        return text
    for key, val in matrix_values.items():
        text = text.replace(f"${{{{ matrix.{key} }}}}", str(val))
    return text


def expand_github_vars(text, context):
    """Replace common ${{ github.X }} expressions for local use."""
    if not isinstance(text, str):
        return text
    base = context.get("base_branch", "master")
    # For changelog check: replace PR base/head sha with git equivalents
    text = text.replace(
        '${{ github.event.pull_request.base.sha }}',
        f"$(git merge-base {base} HEAD)"
    )
    text = text.replace(
        '${{ github.event.pull_request.head.sha }}',
        "$(git rev-parse HEAD)"
    )
    # Strip any remaining ${{ ... }} that we can't resolve
    text = re.sub(r'\$\{\{[^}]*\}\}', '', text)
    return text


def evaluate_condition(if_expr, context):
    """Best-effort evaluation of GitHub Actions `if:` conditions."""
    if not if_expr:
        return True
    s = str(if_expr)
    # PR-only jobs
    if "github.event_name == 'pull_request'" in s:
        return context.get("is_pr", False)
    # master/ci branch only
    if "github.ref_name == 'master'" in s or "refs/heads/master" in s:
        return context.get("is_master", False)
    # Matrix conditionals like matrix.arch != runner.arch
    if "matrix.arch" in s and "runner.arch" in s:
        # For local runs, assume native arch
        if "!=" in s:
            return False  # native == native, so != is false
        if "==" in s:
            return True
    # inputs.additional-setup
    if "inputs.additional-setup" in s:
        return True  # we always want to run setup
    return True


def parse_workflow(workflow_path, reusable_dir=None):
    """Parse a GitHub Actions workflow YAML into a job dict."""
    with open(workflow_path) as f:
        wf = yaml.safe_load(f)

    jobs = {}
    for job_id, job_def in wf.get("jobs", {}).items():
        jobs[job_id] = job_def

    return jobs


def resolve_reusable_workflow(job_def, root):
    """If a job uses a reusable workflow, inline its steps."""
    uses = job_def.get("uses", "")
    if not uses.startswith("./"):
        return None

    wf_path = os.path.join(root, uses)
    if not os.path.exists(wf_path):
        return None

    with open(wf_path) as f:
        reusable = yaml.safe_load(f)

    # Get the inputs passed via `with:`
    inputs = job_def.get("with", {})

    # Reusable workflows have a single job typically
    for rjob_id, rjob_def in reusable.get("jobs", {}).items():
        steps = []
        for step in rjob_def.get("steps", []):
            # Expand input references
            if "run" in step:
                run_cmd = step["run"]
                for key, val in inputs.items():
                    run_cmd = run_cmd.replace(f"${{{{ inputs.{key} }}}}", str(val))
                step = dict(step, run=run_cmd)
            steps.append(step)
        return {
            "name": job_def.get("name", rjob_id),
            "runs-on": inputs.get("os", rjob_def.get("runs-on", "ubuntu-22.04")),
            "steps": steps,
            "needs": job_def.get("needs", []),
        }
    return None


def extract_run_steps(job_def, context):
    """Extract executable (name, script) pairs from a job definition."""
    steps = []
    matrix_values = context.get("matrix", {})

    for step in job_def.get("steps", []):
        # Check if: condition
        if not evaluate_condition(step.get("if"), context):
            continue

        if should_skip_step(step):
            continue

        if "run" not in step:
            continue

        script = step["run"]
        name = step.get("name", script.strip().split("\n")[0][:60])

        # Expand variables
        script = expand_matrix_vars(script, matrix_values)
        script = expand_github_vars(script, context)
        name = expand_matrix_vars(name, matrix_values)

        # Skip powershell steps on non-Windows
        if step.get("shell") == "pwsh" and current_os() != "windows":
            continue

        steps.append((name, script.strip()))

    return steps


def local_jobs():
    """Return a conservative job count to avoid OOM from concurrent linker instances.

    Prefers CARGO_BUILD_JOBS from the environment (set by ci-local).
    Falls back to quarter of CPU cores if not set.
    """
    env_jobs = os.environ.get("CARGO_BUILD_JOBS")
    if env_jobs:
        return int(env_jobs)
    return max(1, multiprocessing.cpu_count() // 4)


def run_script(name, script, root, dry_run, verbose):
    """Run a shell script. Returns (success, output)."""
    if dry_run:
        print(f"  {dim('▸')} {name}")
        for line in script.split("\n"):
            print(f"    {dim(line)}")
        return True, ""

    step_start = time.time()
    sys.stdout.write(f"  {dim('▸')} {name} ... ")
    sys.stdout.flush()

    # Limit parallelism to avoid freezing the machine
    jobs = str(local_jobs())
    env = os.environ.copy()
    env.setdefault("CARGO_BUILD_JOBS", jobs)
    env.setdefault("RUST_TEST_THREADS", jobs)

    try:
        result = subprocess.run(
            ["bash", "-ec", script],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=600,
            env=env,
        )
        elapsed = time.time() - step_start
        output = result.stdout + result.stderr

        if result.returncode == 0:
            print(f"{green('✓')} {dim(f'({elapsed:.1f}s)')}")
            if verbose and output.strip():
                for line in output.strip().split("\n"):
                    print(f"    {dim(line)}")
            return True, output
        else:
            print(f"{red('✗')} {dim(f'({elapsed:.1f}s)')}")
            if output.strip():
                print()
                for line in output.strip().split("\n"):
                    print(f"    {line}")
                print()
            return False, output

    except subprocess.TimeoutExpired:
        print(f"{red('TIMEOUT')} (10 minutes)")
        return False, "TIMEOUT"


def run_job(job_id, job_def, root, context, dry_run, verbose):
    """Run all steps for a job. Returns True on success."""
    name = job_def.get("name", job_id)
    runs_on = job_def.get("runs-on", "")

    print(f"\n{'='*60}")
    print(f"  {bold(job_id)}: {name}")
    print(f"{'='*60}")

    # Platform check
    cur = current_os()
    if not runs_on_matches(runs_on, cur):
        print(f"  {yellow('SKIP')} (requires {runs_on}, running on {cur})")
        return True

    steps = extract_run_steps(job_def, context)
    if not steps:
        print(f"  {dim('(no local steps to run)')}")
        return True

    start = time.time()
    for step_name, script in steps:
        ok, _ = run_script(step_name, script, root, dry_run, verbose)
        if not ok:
            return False

    elapsed = time.time() - start
    if not dry_run:
        print(f"  {green('All steps passed')} {dim(f'({elapsed:.1f}s)')}")
    return True


def main():
    parser = argparse.ArgumentParser(
        description="Local CI runner — executes GitHub workflow jobs locally",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("jobs", nargs="*", help="Jobs to run (default: lint + test)")
    parser.add_argument("--base", default="master", help="Base branch for changelog check")
    parser.add_argument("--dry-run", action="store_true", help="Print commands without executing")
    parser.add_argument("-v", "--verbose", action="store_true", help="Show output on success")
    parser.add_argument("--all", action="store_true", help="Run all jobs viable on this platform")
    parser.add_argument("--workflow", default=".github/workflows/rust.yml",
                        help="Path to workflow YAML")
    parser.add_argument("--pr", action="store_true",
                        help="Simulate PR context (enables changelog check)")
    parser.add_argument("--master", action="store_true",
                        help="Simulate master branch context (enables packaging)")
    parser.add_argument("-j", "--parallel", type=int, default=0, metavar="N",
                        help="Max parallel jobs (default: half of CPU cores)")
    args = parser.parse_args()

    if args.parallel > 0:
        os.environ["CARGO_BUILD_JOBS"] = str(args.parallel)
        os.environ["RUST_TEST_THREADS"] = str(args.parallel)

    root = repo_root()
    if not root:
        print("Error: not in a git repository")
        sys.exit(1)

    os.chdir(root)

    workflow_path = os.path.join(root, args.workflow)
    if not os.path.exists(workflow_path):
        print(f"Error: workflow not found at {workflow_path}")
        sys.exit(1)

    raw_jobs = parse_workflow(workflow_path)
    cur = current_os()

    context = {
        "base_branch": args.base,
        "is_pr": args.pr,
        "is_master": args.master,
        "matrix": {"arch": native_arch()},
    }

    # Resolve reusable workflows and build final job list
    jobs = {}
    for job_id, job_def in raw_jobs.items():
        if "uses" in job_def and job_def["uses"].startswith("./"):
            resolved = resolve_reusable_workflow(job_def, root)
            if resolved:
                jobs[job_id] = resolved
                continue
        jobs[job_id] = job_def

    # Handle 'list' command
    if args.jobs == ["list"]:
        print(f"Jobs in {args.workflow}:\n")
        for job_id, job_def in jobs.items():
            name = job_def.get("name", job_id)
            runs_on = job_def.get("runs-on", "?")
            compatible = runs_on_matches(runs_on, cur)
            marker = green("●") if compatible else dim("○")
            needs = job_def.get("needs", [])
            if isinstance(needs, str):
                needs = [needs]
            deps = f" (needs: {', '.join(needs)})" if needs else ""
            cond = ""
            if job_def.get("if"):
                cond = f" [if: {job_def['if'][:50]}]"
            steps = extract_run_steps(job_def, context)
            step_count = f"{len(steps)} step{'s' if len(steps) != 1 else ''}"
            print(f"  {marker} {bold(job_id):25s} {name:30s} {dim(step_count)}{dim(deps)}{dim(cond)}")
        print(f"\n  {green('●')} = compatible with {cur}   {dim('○')} = other platform")
        return

    # Determine which jobs to run
    if args.all:
        # All jobs that match the current platform (respecting conditions)
        requested = [
            jid for jid, jdef in jobs.items()
            if runs_on_matches(jdef.get("runs-on", ""), cur)
            and evaluate_condition(jdef.get("if"), context)
        ]
    elif args.jobs:
        requested = args.jobs
        # Validate
        for j in requested:
            if j not in jobs:
                print(f"{red('Error')}: Unknown job '{j}'")
                print(f"Available: {', '.join(jobs.keys())}")
                sys.exit(1)
    else:
        # Default: lint + platform test
        requested = []
        if "lint" in jobs:
            requested.append("lint")
        # Find the test job for current platform
        for jid in jobs:
            if cur == "linux" and jid == "linux-test":
                requested.append(jid)
            elif cur == "macos" and jid == "macos-test":
                requested.append(jid)
            elif cur == "windows" and jid == "windows-test":
                requested.append(jid)
        # Android if available
        if cur == "linux" and "android" in jobs:
            requested.append("android")

    if not requested:
        print("No jobs to run.")
        return

    # Topological sort respecting `needs:`
    def topo_sort(job_ids):
        ordered = []
        visited = set()
        def visit(jid):
            if jid in visited:
                return
            visited.add(jid)
            needs = jobs.get(jid, {}).get("needs", [])
            if isinstance(needs, str):
                needs = [needs]
            for dep in needs:
                if dep in job_ids:
                    visit(dep)
            ordered.append(jid)
        for jid in job_ids:
            visit(jid)
        return ordered

    ordered = topo_sort(requested)

    # Run
    jobs_count = args.parallel if args.parallel > 0 else local_jobs()
    print(f"{bold('notedeck local CI')}")
    print(f"Platform: {cur}, Arch: {native_arch()}, Parallelism: {jobs_count}")
    print(f"Jobs: {', '.join(ordered)}")
    if args.dry_run:
        print(f"{yellow('DRY RUN MODE')}")

    results = {}
    failed = False

    for job_id in ordered:
        if failed:
            results[job_id] = "skipped"
            continue

        ok = run_job(job_id, jobs[job_id], root, context, args.dry_run, args.verbose)
        results[job_id] = "passed" if ok else "failed"
        if not ok:
            failed = True

    # Summary
    print(f"\n{'='*60}")
    print(f"  {bold('Summary')}")
    print(f"{'='*60}")
    for job_id, status in results.items():
        if status == "passed":
            icon = green("✓")
        elif status == "failed":
            icon = red("✗")
        else:
            icon = yellow("○")
        print(f"  {icon} {job_id}")

    if failed:
        print(f"\n{red('CI failed')}")
        sys.exit(1)
    else:
        print(f"\n{green('All checks passed')}")


if __name__ == "__main__":
    main()
