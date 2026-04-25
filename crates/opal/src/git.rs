use anyhow::{Context, Result, anyhow};
use git2::{DiffOptions, Repository, Status, StatusOptions};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn open_repository(workdir: &Path) -> Result<Repository> {
    Repository::discover(workdir)
        .with_context(|| format!("failed to open git repository from {}", workdir.display()))
}

pub fn repository_root(workdir: &Path) -> Result<PathBuf> {
    let repo = open_repository(workdir)?;
    if let Some(root) = repo.workdir() {
        return Ok(root.to_path_buf());
    }
    repo.path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("repository has no working directory"))
}

pub fn repository_common_dir(workdir: &Path) -> Result<PathBuf> {
    let repo = open_repository(workdir)?;
    Ok(repo.commondir().to_path_buf())
}

fn resolve_commit<'repo>(repo: &'repo Repository, spec: &str) -> Result<git2::Commit<'repo>> {
    repo.revparse_single(spec)
        .with_context(|| format!("failed to resolve git revision '{spec}'"))?
        .peel_to_commit()
        .with_context(|| format!("revision '{spec}' does not point to a commit"))
}

pub fn current_branch(workdir: &Path) -> Result<String> {
    let repo = open_repository(workdir)?;
    let head = repo.head().context("failed to read HEAD")?;
    if !head.is_branch() {
        return Err(anyhow!("HEAD is not attached to a branch"));
    }
    head.shorthand()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("HEAD branch has no shorthand name"))
}

pub fn head_ref(workdir: &Path) -> Result<String> {
    let repo = open_repository(workdir)?;
    Ok(repo
        .head()
        .context("failed to read HEAD")?
        .peel_to_commit()
        .context("HEAD does not point to a commit")?
        .id()
        .to_string())
}

pub fn current_tag(workdir: &Path) -> Result<String> {
    let repo = open_repository(workdir)?;
    let head = repo
        .head()
        .context("failed to read HEAD")?
        .peel_to_commit()
        .context("HEAD does not point to a commit")?
        .id();

    let mut tags = Vec::new();
    for reference in repo
        .references_glob("refs/tags/*")
        .context("failed to enumerate tags")?
    {
        let reference = reference.context("failed to read tag reference")?;
        let Some(name) = reference.shorthand() else {
            continue;
        };
        let Ok(commit) = reference.peel_to_commit() else {
            continue;
        };
        if commit.id() == head {
            tags.push(name.to_string());
        }
    }

    tags.sort();
    if tags.is_empty() {
        return Err(anyhow!("no tag points at HEAD"));
    }
    if tags.len() > 1 {
        return Err(anyhow!(
            "multiple tags point at HEAD: {}; set CI_COMMIT_TAG or GIT_COMMIT_TAG explicitly",
            tags.join(", ")
        ));
    }
    Ok(tags.remove(0))
}

pub fn merge_base(workdir: &Path, base: &str, head: Option<&str>) -> Result<Option<String>> {
    let repo = open_repository(workdir)?;
    let Ok(base) = resolve_commit(&repo, base) else {
        return Ok(None);
    };
    let Ok(head) = resolve_commit(&repo, head.unwrap_or("HEAD")) else {
        return Ok(None);
    };

    match repo.merge_base(base.id(), head.id()) {
        Ok(oid) => Ok(Some(oid.to_string())),
        Err(err) if err.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(err) => Err(err).context("failed to compute merge base"),
    }
}

pub fn default_branch(workdir: &Path) -> Result<String> {
    let repo = open_repository(workdir)?;
    let reference = repo
        .find_reference("refs/remotes/origin/HEAD")
        .context("failed to read refs/remotes/origin/HEAD")?;
    let target = reference
        .symbolic_target()
        .ok_or_else(|| anyhow!("origin HEAD is not a symbolic reference"))?;
    target
        .rsplit('/')
        .next()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("origin HEAD target has no branch segment"))
}

pub fn changed_files(
    workdir: &Path,
    base: Option<&str>,
    head: Option<&str>,
) -> Result<HashSet<String>> {
    let repo = match open_repository(workdir) {
        Ok(repo) => repo,
        Err(_) => return Ok(HashSet::new()),
    };
    let mut opts = DiffOptions::new();

    let diff = match (base, head) {
        (Some(base), Some(head)) => {
            let Ok(base) = resolve_commit(&repo, base) else {
                return Ok(HashSet::new());
            };
            let Ok(head) = resolve_commit(&repo, head) else {
                return Ok(HashSet::new());
            };
            let base_tree = base.tree().context("failed to read base tree")?;
            let head_tree = head.tree().context("failed to read head tree")?;
            repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut opts))
                .context("failed to diff git trees")?
        }
        (Some(base), None) => {
            let Ok(base) = resolve_commit(&repo, base) else {
                return Ok(HashSet::new());
            };
            let head = repo
                .head()
                .context("failed to read HEAD")?
                .peel_to_commit()
                .context("HEAD does not point to a commit")?;
            let base_tree = base.tree().context("failed to read base tree")?;
            let head_tree = head.tree().context("failed to read head tree")?;
            repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut opts))
                .context("failed to diff git trees")?
        }
        (None, Some(head)) => {
            let Ok(head) = resolve_commit(&repo, head) else {
                return Ok(HashSet::new());
            };
            let head_tree = head.tree().context("failed to read head tree")?;
            repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut opts))
                .context("failed to diff git tree against workdir")?
        }
        (None, None) => {
            let head = repo
                .head()
                .context("failed to read HEAD")?
                .peel_to_commit()
                .context("HEAD does not point to a commit")?;
            let Ok(parent) = head.parent(0) else {
                return Ok(HashSet::new());
            };
            let parent_tree = parent.tree().context("failed to read parent tree")?;
            let head_tree = head.tree().context("failed to read head tree")?;
            repo.diff_tree_to_tree(Some(&parent_tree), Some(&head_tree), Some(&mut opts))
                .context("failed to diff git trees")?
        }
    };

    let mut paths = HashSet::new();
    for delta in diff.deltas() {
        if let Some(path) = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .and_then(path_to_string)
        {
            paths.insert(path);
        }
    }
    Ok(paths)
}

pub fn untracked_files(workdir: &Path) -> Result<Vec<String>> {
    let repo = open_repository(workdir)?;
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(true)
        .include_unmodified(false);
    let statuses = repo
        .statuses(Some(&mut opts))
        .context("failed to enumerate git status entries")?;

    let mut paths = Vec::new();
    for entry in statuses.iter() {
        let status = entry.status();
        if !status_intersects_untracked(status) {
            continue;
        }
        if let Some(path) = entry.path() {
            paths.push(path.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn status_intersects_untracked(status: Status) -> bool {
    status.is_wt_new() || status.is_ignored()
}

fn path_to_string(path: &Path) -> Option<String> {
    path.to_str().map(str::to_string)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use anyhow::Result;
    use git2::{RepositoryInitOptions, Signature};
    use tempfile::{TempDir, tempdir};

    pub(crate) fn init_repo_with_commit_and_tag(tag: &str) -> Result<TempDir> {
        let dir = tempdir()?;
        let mut init = RepositoryInitOptions::new();
        init.initial_head("main");
        let repo = Repository::init_opts(dir.path(), &init)?;

        std::fs::write(dir.path().join("README.md"), "opal\n")?;

        let mut index = repo.index()?;
        index.add_path(Path::new("README.md"))?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = Signature::now("Opal Tests", "opal@example.com")?;
        let oid = repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])?;
        let object = repo.find_object(oid, None)?;
        repo.tag_lightweight(tag, &object, false)?;

        Ok(dir)
    }

    pub(crate) fn init_repo_with_commit_and_tags(tags: &[&str]) -> Result<TempDir> {
        let dir = tempdir()?;
        let mut init = RepositoryInitOptions::new();
        init.initial_head("main");
        let repo = Repository::init_opts(dir.path(), &init)?;

        std::fs::write(dir.path().join("README.md"), "opal\n")?;

        let mut index = repo.index()?;
        index.add_path(Path::new("README.md"))?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = Signature::now("Opal Tests", "opal@example.com")?;
        let oid = repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])?;
        let object = repo.find_object(oid, None)?;
        for tag in tags {
            repo.tag_lightweight(tag, &object, false)?;
        }

        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::{current_tag, test_support::init_repo_with_commit_and_tags, untracked_files};
    use anyhow::Result;
    use git2::{RepositoryInitOptions, Signature};
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn untracked_files_include_ignored_paths() -> Result<()> {
        let dir = tempdir()?;
        let mut init = RepositoryInitOptions::new();
        init.initial_head("main");
        let repo = git2::Repository::init_opts(dir.path(), &init)?;

        std::fs::write(dir.path().join("README.md"), "opal\n")?;
        std::fs::write(dir.path().join(".gitignore"), "tests-temp/\n")?;

        let mut index = repo.index()?;
        index.add_path(Path::new("README.md"))?;
        index.add_path(Path::new(".gitignore"))?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = Signature::now("Opal Tests", "opal@example.com")?;
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])?;

        std::fs::create_dir_all(dir.path().join("tests-temp"))?;
        std::fs::write(dir.path().join("tests-temp").join("generated.txt"), "hi")?;
        std::fs::write(dir.path().join("scratch.txt"), "hello")?;

        let files = untracked_files(dir.path())?;

        assert!(files.iter().any(|path| path == "scratch.txt"));
        assert!(files.iter().any(|path| path == "tests-temp/generated.txt"));
        Ok(())
    }

    #[test]
    fn current_tag_errors_when_multiple_tags_point_to_head() -> Result<()> {
        let dir = init_repo_with_commit_and_tags(&["v0.1.2", "v0.1.3"])?;
        let err = current_tag(dir.path()).expect_err("multiple tags should be ambiguous");
        assert!(
            err.to_string().contains("multiple tags point at HEAD"),
            "unexpected error: {err:#}"
        );
        Ok(())
    }
}
