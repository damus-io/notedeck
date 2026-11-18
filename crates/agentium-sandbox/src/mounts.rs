//! Which host directories a guest gets, and where: a checkout and the git
//! directory behind it, each mounted at the **same path in the guest as on
//! the host**.
//!
//! # Why guest == host
//!
//! A linked worktree is held together by absolute paths that point both ways:
//!
//! ```text
//! <wt>/.git                         (file)  gitdir: /abs/main/.git/worktrees/<name>
//! <main>/.git/worktrees/<name>/gitdir       /abs/wt/.git        <- points back
//! <main>/.git/worktrees/<name>/commondir    ../..               (relative)
//! ```
//!
//! Mount only the worktree and git in the guest says `fatal: not a git
//! repository`. Mount the worktree and the main `.git` at their host paths
//! and every one of those pointers resolves unchanged, so status, log,
//! edits and commits all work. It also makes the guest-to-host path mapping
//! the identity: dave draws status and diffs against host paths
//! (`notedeck_dave/src/git_status.rs`, `ui/diff.rs`), and no guest path can
//! leak into a transcript or a commit, because there are none.
//!
//! Don't clone in the guest instead: this repo's `.git` is 557 MB, and a
//! clone per session breaks the shared object store worktrees exist for.
//!
//! # Keep tree-wide git on the host
//!
//! Guest and host see the same directory, and virtiofs metadata is about
//! 10x slower than the host's (`git status --porcelain`: 26.6 ms on the
//! host, 265-418 ms in the guest). So anything that stats the whole working
//! tree (the git status panel, the diff view) keeps running host-side. Only
//! the agent's own tool calls pay the virtiofs tax.
//!
//! # Two profiles
//!
//! | [`GitAccess`] | worktree | git dir | the agent can |
//! |---|---|---|---|
//! | [`Review`](GitAccess::Review) (default) | rw | **ro** | read history, edit and build in the tree |
//! | [`Commit`](GitAccess::Commit) (opt-in) | rw | rw | also `add`, `commit`, `stash`, `checkout -b`, `gc` |
//!
//! A readonly git dir refuses everything that writes the index, refs or
//! objects (`index.lock: Read-only file system`), so the profile is enforced
//! by the mount rather than by matching shell commands.
//!
//! **What neither profile stops is `git push`.** Push reads local objects
//! and writes only to the remote; with a readonly git dir it lands and even
//! exits 0, and only the remote-tracking ref update afterwards fails
//! (`tests/mounts.rs::push_lands_through_a_readonly_git_dir`). What keeps a
//! guest from pushing is that it holds no credentials (no `~/.ssh`, no
//! credential helper) and its egress allowlist, not this module.
//!
//! The writes are real: a bind mount passes straight through to the host,
//! so the blast radius is this worktree, plus the whole repo's object store
//! under [`Commit`](GitAccess::Commit).

use std::fs;
use std::path::{Component, Path, PathBuf};

/// How much of the repository's shared git state the guest may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitAccess {
    /// The git directory is mounted readonly: the agent reads history and
    /// edits the tree, but can't stage, commit or rewrite refs.
    #[default]
    Review,
    /// The git directory is mounted read-write. Opt-in, and the user should
    /// be able to see a session has it.
    Commit,
}

/// One host directory, bind-mounted into the guest at the same path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The path on the host and in the guest. Absolute and lexically
    /// normal (no `.` or `..`), spelled the way git spells it, which is
    /// what the guest will look up.
    pub path: PathBuf,
    /// Writes fail with `EROFS` in the guest.
    pub readonly: bool,
}

/// What kind of checkout a directory is, as git sees it from inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoLayout {
    /// No `.git` at the top of the directory: it's mounted on its own.
    NotARepo,
    /// `.git` is a directory inside the checkout.
    MainCheckout {
        /// `<checkout>/.git`.
        git_dir: PathBuf,
    },
    /// `.git` is a file pointing somewhere else: a linked worktree (or a
    /// submodule).
    Linked {
        /// Where `.git` points: `<main>/.git/worktrees/<name>` for a
        /// worktree.
        git_dir: PathBuf,
        /// The repository's shared git directory, from `git_dir/commondir`
        /// (`git_dir` itself when there isn't one, as for a submodule).
        common_dir: PathBuf,
    },
}

/// One environment variable every command in the guest gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestEnv {
    /// Its name.
    pub key: &'static str,
    /// Its value.
    pub value: String,
}

/// The mounts that give a guest a working checkout at its host path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPlan {
    /// The checkout, absolute and lexically normal. The guest's working
    /// directory.
    pub checkout: PathBuf,
    /// What the checkout turned out to be.
    pub layout: RepoLayout,
    /// The profile the plan was made for.
    pub access: GitAccess,
    /// Bind mounts in the order to apply them: a directory always comes
    /// before any mount nested inside it.
    pub mounts: Vec<Mount>,
}

/// Why a checkout couldn't be planned.
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    /// The checkout isn't a directory we can read.
    #[error("{path}: {source}")]
    Io {
        /// The path being read.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
    /// The guest is given paths as strings, so a path must be UTF-8.
    #[error("{0}: not valid UTF-8, can't be mounted in the guest")]
    NotUtf8(PathBuf),
    /// A `.git` file that doesn't start with `gitdir: `.
    #[error("{0}: a .git file without a gitdir: line")]
    BadGitFile(PathBuf),
    /// The `.git` file points at a directory that isn't there: a worktree
    /// whose main repository moved or was deleted.
    #[error("{checkout}: .git points at {git_dir}, which doesn't exist")]
    MissingGitDir {
        /// The checkout.
        checkout: PathBuf,
        /// Where its `.git` file points.
        git_dir: PathBuf,
    },
}

impl MountPlan {
    /// Plan the mounts for `checkout` (a directory, normally a dave
    /// session's cwd) under `access`.
    ///
    /// Reads `<checkout>/.git` and, for a linked worktree, the `commondir`
    /// file behind it, and nothing else. Paths are kept as written rather
    /// than canonicalised, so a checkout reached through a symlink is
    /// mounted at the path dave knows it by.
    pub fn for_checkout(checkout: &Path, access: GitAccess) -> Result<Self, MountError> {
        let checkout = absolute(checkout)?;
        let dot_git = checkout.join(".git");
        let layout = read_layout(&checkout, &dot_git)?;
        let mounts = plan_mounts(&checkout, &layout, access);
        for m in &mounts {
            if m.path.to_str().is_none() {
                return Err(MountError::NotUtf8(m.path.clone()));
            }
        }
        Ok(MountPlan {
            checkout,
            layout,
            access,
            mounts,
        })
    }

    /// The checkout as a string, for the guest's working directory.
    pub fn checkout_str(&self) -> &str {
        // for_checkout refused any non-UTF-8 mount, and the checkout is
        // always one of them.
        self.checkout.to_str().unwrap_or_default()
    }

    /// The environment that makes git in the guest trust the checkout.
    ///
    /// The mounted files belong to the host user, and the guest runs as
    /// its image's user (root, for the default image), so git would refuse
    /// the repository as `dubious ownership`. `safe.directory` is only read
    /// from protected config: system, global or the command line. The
    /// `GIT_CONFIG_COUNT` variables are the command line's, so they work
    /// without touching any file in the image, and they name this checkout
    /// only rather than `*`. Empty when the checkout isn't a repository.
    pub fn git_env(&self) -> Vec<GuestEnv> {
        if self.layout == RepoLayout::NotARepo {
            return Vec::new();
        }
        vec![
            GuestEnv {
                key: "GIT_CONFIG_COUNT",
                value: "1".to_owned(),
            },
            GuestEnv {
                key: "GIT_CONFIG_KEY_0",
                value: "safe.directory".to_owned(),
            },
            GuestEnv {
                key: "GIT_CONFIG_VALUE_0",
                value: self.checkout_str().to_owned(),
            },
        ]
    }

    /// Add the plan's bind mounts to a sandbox, start it in the checkout
    /// and have git trust it ([`MountPlan::git_env`]).
    #[cfg(microvm)]
    pub fn apply(
        &self,
        builder: microsandbox::sandbox::SandboxBuilder,
    ) -> microsandbox::sandbox::SandboxBuilder {
        let builder = self
            .git_env()
            .into_iter()
            .fold(builder, |b, e| b.env(e.key, e.value));
        let builder = self.mounts.iter().fold(builder, |b, m| {
            let path = m.path.to_string_lossy().into_owned();
            let readonly = m.readonly;
            b.volume(path, |v| {
                let v = v.bind(&m.path);
                if readonly {
                    v.readonly()
                } else {
                    v
                }
            })
        });
        builder.workdir(self.checkout_str())
    }
}

/// Read what `<checkout>/.git` is.
fn read_layout(checkout: &Path, dot_git: &Path) -> Result<RepoLayout, MountError> {
    let meta = match fs::metadata(dot_git) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let checkout_meta = fs::metadata(checkout).map_err(|source| MountError::Io {
                path: checkout.to_owned(),
                source,
            })?;
            if !checkout_meta.is_dir() {
                return Err(MountError::Io {
                    path: checkout.to_owned(),
                    source: std::io::Error::other("not a directory"),
                });
            }
            return Ok(RepoLayout::NotARepo);
        }
        Err(source) => {
            return Err(MountError::Io {
                path: dot_git.to_owned(),
                source,
            })
        }
    };

    if meta.is_dir() {
        return Ok(RepoLayout::MainCheckout {
            git_dir: dot_git.to_owned(),
        });
    }

    let text = read(dot_git)?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| MountError::BadGitFile(dot_git.to_owned()))?;
    // Relative since git 2.48's worktree.useRelativePaths: relative to the
    // checkout, where the .git file sits.
    let git_dir = normalize(&checkout.join(target));
    if !git_dir.is_dir() {
        return Err(MountError::MissingGitDir {
            checkout: checkout.to_owned(),
            git_dir,
        });
    }

    let commondir = git_dir.join("commondir");
    let common_dir = match fs::read_to_string(&commondir) {
        Ok(text) => normalize(&git_dir.join(text.trim())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
        Err(source) => {
            return Err(MountError::Io {
                path: commondir,
                source,
            })
        }
    };
    Ok(RepoLayout::Linked {
        git_dir,
        common_dir,
    })
}

/// The mounts for a layout: the checkout read-write, then whatever git dir
/// lives outside it (or, under [`GitAccess::Review`], inside it) at the
/// profile's access.
fn plan_mounts(checkout: &Path, layout: &RepoLayout, access: GitAccess) -> Vec<Mount> {
    let git_readonly = access == GitAccess::Review;
    let mut mounts = vec![Mount {
        path: checkout.to_owned(),
        readonly: false,
    }];
    let mut add_git = |path: &Path| {
        // A git dir inside a read-write mount needs its own mount only to
        // make it readonly; a read-write one is already covered.
        let covered = mounts.iter().any(|m| path.starts_with(&m.path));
        if covered && !git_readonly {
            return;
        }
        if mounts.iter().any(|m| m.path == path) {
            return;
        }
        mounts.push(Mount {
            path: path.to_owned(),
            readonly: git_readonly,
        });
    };

    match layout {
        RepoLayout::NotARepo => {}
        RepoLayout::MainCheckout { git_dir } => add_git(git_dir),
        RepoLayout::Linked {
            git_dir,
            common_dir,
        } => {
            add_git(common_dir);
            // A worktree's own git dir is inside the common one; a
            // submodule-style gitdir without a commondir is the common one.
            if !git_dir.starts_with(common_dir) {
                add_git(git_dir);
            }
        }
    }

    // Parents before children, so a nested readonly mount lands on top of
    // the read-write one that contains it.
    mounts.sort_by_key(|m| m.path.components().count());
    mounts
}

fn read(path: &Path) -> Result<String, MountError> {
    fs::read_to_string(path).map_err(|source| MountError::Io {
        path: path.to_owned(),
        source,
    })
}

/// `path` made absolute against the current directory and lexically
/// normalised.
fn absolute(path: &Path) -> Result<PathBuf, MountError> {
    let abs = std::path::absolute(path).map_err(|source| MountError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(normalize(&abs))
}

/// Drop `.` and resolve `..` lexically, without touching the filesystem.
///
/// Lexical is right here rather than `canonicalize`: the guest has these
/// paths as plain mount points with no symlinks on them, so `..` in the
/// guest goes to the lexical parent, and git spells the paths this way.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn normalize_resolves_dots_lexically() {
        assert_eq!(normalize(&p("/a/.git/worktrees/wt/../..")), p("/a/.git"));
        assert_eq!(normalize(&p("/a/./b/../c")), p("/a/c"));
        assert_eq!(normalize(&p("/../a")), p("/a"));
    }

    #[test]
    fn linked_review_mounts_the_common_dir_readonly() {
        let layout = RepoLayout::Linked {
            git_dir: p("/r/main/.git/worktrees/wt"),
            common_dir: p("/r/main/.git"),
        };
        let mounts = plan_mounts(&p("/r/wt"), &layout, GitAccess::Review);
        assert_eq!(
            mounts,
            vec![
                Mount {
                    path: p("/r/wt"),
                    readonly: false
                },
                Mount {
                    path: p("/r/main/.git"),
                    readonly: true
                },
            ]
        );
    }

    #[test]
    fn linked_commit_mounts_the_common_dir_read_write() {
        let layout = RepoLayout::Linked {
            git_dir: p("/r/main/.git/worktrees/wt"),
            common_dir: p("/r/main/.git"),
        };
        let mounts = plan_mounts(&p("/r/wt"), &layout, GitAccess::Commit);
        assert!(mounts.iter().all(|m| !m.readonly));
        assert_eq!(mounts.len(), 2);
    }

    #[test]
    fn main_checkout_review_nests_a_readonly_git_dir() {
        let layout = RepoLayout::MainCheckout {
            git_dir: p("/r/main/.git"),
        };
        let mounts = plan_mounts(&p("/r/main"), &layout, GitAccess::Review);
        assert_eq!(mounts[0].path, p("/r/main"));
        assert!(!mounts[0].readonly);
        assert_eq!(mounts[1].path, p("/r/main/.git"));
        assert!(mounts[1].readonly);
    }

    #[test]
    fn main_checkout_commit_is_one_mount() {
        let layout = RepoLayout::MainCheckout {
            git_dir: p("/r/main/.git"),
        };
        let mounts = plan_mounts(&p("/r/main"), &layout, GitAccess::Commit);
        assert_eq!(mounts.len(), 1);
    }

    /// A worktree checked out inside the main checkout (`main/.wt/x`): the
    /// mounts are still disjoint, and the main working tree stays out.
    #[test]
    fn worktree_inside_the_main_checkout() {
        let layout = RepoLayout::Linked {
            git_dir: p("/r/main/.git/worktrees/x"),
            common_dir: p("/r/main/.git"),
        };
        let mounts = plan_mounts(&p("/r/main/.wt/x"), &layout, GitAccess::Review);
        let paths: Vec<_> = mounts.iter().map(|m| m.path.clone()).collect();
        assert_eq!(paths, vec![p("/r/main/.git"), p("/r/main/.wt/x")]);
    }

    /// A submodule's `.git` file names a gitdir with no `commondir`; that
    /// gitdir is the one to mount.
    #[test]
    fn gitdir_outside_the_common_dir_is_mounted_too() {
        let layout = RepoLayout::Linked {
            git_dir: p("/r/main/.git/modules/sub"),
            common_dir: p("/r/main/.git/modules/sub"),
        };
        let mounts = plan_mounts(&p("/r/main/sub"), &layout, GitAccess::Review);
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[1].path, p("/r/main/.git/modules/sub"));
    }
}
