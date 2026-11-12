use std::path::{Path, PathBuf};

use git2::{Oid, Repository, RepositoryInitOptions, Signature};
use tempfile::TempDir;

pub struct Fixture {
    pub dir: TempDir,
    pub repo: Repository,
}

impl Fixture {
    /// Repository with HEAD on an unborn `main`.
    pub fn unborn() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(dir.path(), &opts).expect("init");
        Self { dir, repo }
    }

    pub fn with_commit() -> Self {
        let fixture = Self::unborn();
        fixture.commit_file("a.txt", "one\n");
        fixture
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    pub fn commit_file(&self, name: &str, content: &str) -> Oid {
        std::fs::write(self.dir.path().join(name), content).expect("write");
        let mut index = self.repo.index().expect("index");
        index.add_path(Path::new(name)).expect("add");
        index.write().expect("index write");
        let tree = self
            .repo
            .find_tree(index.write_tree().expect("tree"))
            .expect("find tree");
        let sig = Signature::now("t", "t@example.com").expect("sig");
        let parents: Vec<_> = self
            .repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<_> = parents.iter().collect();
        self.repo
            .commit(Some("HEAD"), &sig, &sig, "c", &tree, &parent_refs)
            .expect("commit")
    }
}
