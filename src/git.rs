use anyhow::{Context, Result, anyhow};
use git2::{DiffOptions, Repository};
use std::collections::HashSet;
use std::path::Path;

fn open_repository(workdir: &Path) -> Result<Repository> {
    Repository::discover(workdir)
        .with_context(|| format!("failed to open git repository from {}", workdir.display()))
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
    tags.into_iter()
        .next()
        .ok_or_else(|| anyhow!("no tag points at HEAD"))
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
    let repo = open_repository(workdir)?;
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

fn path_to_string(path: &Path) -> Option<String> {
    path.to_str().map(str::to_string)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use git2::{RepositoryInitOptions, Signature};
    use tempfile::{TempDir, tempdir};

    pub(crate) fn init_repo_with_commit_and_tag(tag: &str) -> TempDir {
        let dir = tempdir().expect("tempdir");
        let mut init = RepositoryInitOptions::new();
        init.initial_head("main");
        let repo = Repository::init_opts(dir.path(), &init).expect("init repository");

        std::fs::write(dir.path().join("README.md"), "opal\n").expect("write README");

        let mut index = repo.index().expect("open index");
        index
            .add_path(Path::new("README.md"))
            .expect("add README to index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = Signature::now("Opal Tests", "opal@example.com").expect("signature");
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .expect("create commit");
        let object = repo.find_object(oid, None).expect("find commit object");
        repo.tag_lightweight(tag, &object, false)
            .expect("create lightweight tag");

        dir
    }
}
