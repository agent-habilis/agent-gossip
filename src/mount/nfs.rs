use std::collections::HashMap;

use anyhow::{Result, bail};
use async_trait::async_trait;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

use crate::file::walk::safe_component;

use super::MAX_READ_LEN;
use super::wire::MountManifest;

/// Where the file bytes come from — a seam so [`RemoteFs`] unit-tests against
/// an in-memory source instead of a live iroh connection.
#[async_trait]
pub(super) trait ByteSource: Send + Sync {
    /// Fetch up to `len` bytes of manifest file `index` starting at `offset`.
    /// A short return means EOF was reached.
    async fn read(&self, index: u32, offset: u64, len: u32) -> Result<Vec<u8>>;
}

/// One inode of the mounted tree. The vector index of a node **is** its NFS
/// fileid (nfsserve reserves fileid 0, so slot 0 is a never-served filler and
/// the root lives at 1) — no id map to keep in sync.
pub(super) enum Node {
    Dir {
        name: String,
        /// The parent's fileid — root points at itself. Needed for `..`.
        parent: fileid3,
        mode: u32,
        mtime: i64,
        /// Child fileids, sorted by name for deterministic `readdir`.
        children: Vec<fileid3>,
    },
    File {
        name: String,
        /// Position in the manifest's file list — the wire READ index.
        file_index: u32,
        size: u64,
        mode: u32,
        mtime: i64,
    },
}

/// Root fileid; slot 0 is reserved by the nfsserve cookie scheme.
const ROOT_ID: fileid3 = 1;

/// Build the inode table from a received manifest. Every path component is
/// validated with [`safe_component`] — the manifest is attacker-controlled
/// input even though it never touches the local disk (a hostile name could
/// otherwise confuse everything that displays or resolves it).
///
/// # Errors
/// A path with an unsafe component, a duplicate entry, or a file used as a
/// directory.
pub(super) fn build_tree(manifest: &MountManifest) -> Result<Vec<Node>> {
    let mut builder = TreeBuilder::new();

    for dir in &manifest.dirs {
        validate_rel_path(&dir.rel_path)?;
        let id = builder.ensure_dir(&dir.rel_path)?;
        // The scan lists parents before children, but re-set the attrs anyway
        // in case this dir was first synthesized as a missing parent.
        let index = usize::try_from(id).expect("fileid is a vec index");
        if let Node::Dir { mode, mtime, .. } = &mut builder.nodes[index] {
            *mode = dir.mode;
            *mtime = dir.mtime;
        }
    }

    for (position, file) in manifest.files.iter().enumerate() {
        validate_rel_path(&file.rel_path)?;
        let (parent_path, name) = split_parent(&file.rel_path);
        let parent = builder.ensure_dir(parent_path)?;
        if builder.child(parent, name).is_some() {
            bail!("duplicate manifest entry: {}", file.rel_path);
        }
        builder.push(
            parent,
            Node::File {
                name: name.to_owned(),
                file_index: u32::try_from(position).expect("file count fits u32"),
                size: file.size,
                mode: file.mode,
                mtime: file.mtime,
            },
        );
    }

    let mut nodes = builder.nodes;
    sort_children(&mut nodes);
    Ok(nodes)
}

/// Reject a manifest path with any unsafe component. Splitting on `/` also
/// surfaces the empty component hidden in a leading `/` or a doubled `//`,
/// which the parent-splitting alone would let through.
fn validate_rel_path(path: &str) -> Result<()> {
    if path.is_empty() {
        bail!("empty path in manifest");
    }
    for component in path.split('/') {
        safe_component(component)?;
    }
    Ok(())
}

/// The under-construction inode table plus the name indexes that keep every
/// insert O(1) — the manual sells mount for huge trees, so tree build must
/// not scan siblings per entry.
struct TreeBuilder {
    nodes: Vec<Node>,
    /// `rel_path` → fileid for every directory seen so far ("" is the root).
    dir_ids: HashMap<String, fileid3>,
    /// Per-directory name → child fileid, for duplicate checks while
    /// building (serve-time lookup uses the sorted children instead).
    names: HashMap<fileid3, HashMap<String, fileid3>>,
}

impl TreeBuilder {
    fn new() -> Self {
        let nodes = vec![
            // Slot 0: reserved, never served.
            Node::Dir {
                name: String::new(),
                parent: 0,
                mode: 0,
                mtime: 0,
                children: Vec::new(),
            },
            Node::Dir {
                name: String::new(),
                parent: ROOT_ID,
                mode: 0o755,
                mtime: 0,
                children: Vec::new(),
            },
        ];
        Self {
            nodes,
            dir_ids: HashMap::from([(String::new(), ROOT_ID)]),
            names: HashMap::new(),
        }
    }

    /// The fileid of directory `path`, creating it (and any missing parents)
    /// as it goes. `""` is the root. Iterative on purpose: the wire allows
    /// a `rel_path` up to `64 KiB` (~32k components), so a hostile manifest
    /// must not be able to recurse the stack away.
    fn ensure_dir(&mut self, path: &str) -> Result<fileid3> {
        if let Some(&id) = self.dir_ids.get(path) {
            return Ok(id);
        }
        let mut current = ROOT_ID;
        let mut prefix = String::with_capacity(path.len());
        for component in path.split('/') {
            safe_component(component)?;
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if let Some(&id) = self.dir_ids.get(prefix.as_str()) {
                current = id;
                continue;
            }
            if self.child(current, component).is_some() {
                // The name is taken — by a file, since a dir would be in
                // `dir_ids`.
                bail!("manifest uses file {prefix:?} as a directory");
            }
            current = self.push(
                current,
                Node::Dir {
                    name: component.to_owned(),
                    parent: current,
                    // Attrs for a parent the manifest never listed explicitly.
                    mode: 0o755,
                    mtime: 0,
                    children: Vec::new(),
                },
            );
            self.dir_ids.insert(prefix.clone(), current);
        }
        Ok(current)
    }

    fn child(&self, parent: fileid3, name: &str) -> Option<fileid3> {
        self.names
            .get(&parent)
            .and_then(|siblings| siblings.get(name))
            .copied()
    }

    /// Append `node` under `parent`, indexing its name, and return its fileid.
    fn push(&mut self, parent: fileid3, node: Node) -> fileid3 {
        let id = u64::try_from(self.nodes.len()).expect("node count fits u64");
        self.names
            .entry(parent)
            .or_default()
            .insert(node_name(&node).to_owned(), id);
        self.nodes.push(node);
        if let Node::Dir { children, .. } =
            &mut self.nodes[usize::try_from(parent).expect("vec index")]
        {
            children.push(id);
        }
        id
    }
}

fn split_parent(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    }
}

fn node_name(node: &Node) -> &str {
    match node {
        Node::Dir { name, .. } | Node::File { name, .. } => name,
    }
}

fn sort_children(nodes: &mut [Node]) {
    // Two passes because sorting needs immutable access to sibling names.
    let mut sorted: Vec<(usize, Vec<fileid3>)> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Node::Dir { children, .. } = node {
            let mut ordered = children.clone();
            ordered.sort_by(|&left, &right| {
                node_name(&nodes[usize::try_from(left).expect("vec index")]).cmp(node_name(
                    &nodes[usize::try_from(right).expect("vec index")],
                ))
            });
            sorted.push((index, ordered));
        }
    }
    for (index, ordered) in sorted {
        if let Node::Dir { children, .. } = &mut nodes[index] {
            *children = ordered;
        }
    }
}

/// The read-only remote filesystem served to the local NFS client. All
/// metadata answers come from the in-memory tree (the mount-time snapshot);
/// only `read` touches the network.
pub(super) struct RemoteFs<S> {
    nodes: Vec<Node>,
    source: S,
    uid: u32,
    gid: u32,
}

impl<S: ByteSource> RemoteFs<S> {
    pub(super) fn new(nodes: Vec<Node>, source: S, uid: u32, gid: u32) -> Self {
        Self {
            nodes,
            source,
            uid,
            gid,
        }
    }

    fn node(&self, id: fileid3) -> Result<&Node, nfsstat3> {
        if id == 0 {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }
        usize::try_from(id)
            .ok()
            .and_then(|index| self.nodes.get(index))
            .ok_or(nfsstat3::NFS3ERR_NOENT)
    }

    fn attr(&self, id: fileid3, node: &Node) -> fattr3 {
        // The mode floor (`r` on files, `rx` on dirs) guarantees the mounting
        // user can always read what the producer chose to share, regardless
        // of producer-side permission bits — enforcement is client-side
        // against exactly these attrs.
        let (ftype, mode, size, mtime) = match node {
            Node::Dir { mode, mtime, .. } => (ftype3::NF3DIR, (mode & 0o777) | 0o555, 4096, *mtime),
            Node::File {
                mode, mtime, size, ..
            } => (ftype3::NF3REG, (mode & 0o777) | 0o444, *size, *mtime),
        };
        let time = nfs_time(mtime);
        fattr3 {
            ftype,
            mode,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            size,
            used: size,
            rdev: specdata3::default(),
            fsid: 0,
            fileid: id,
            atime: time,
            mtime: time,
            ctime: time,
        }
    }
}

fn nfs_time(mtime: i64) -> nfstime3 {
    nfstime3 {
        // NFSv3 times are u32 seconds; clamp the pre-1970 / post-2106 fringe.
        seconds: u32::try_from(mtime).unwrap_or(0),
        nseconds: 0,
    }
}

#[async_trait]
impl<S: ByteSource + 'static> NFSFileSystem for RemoteFs<S> {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_ID
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let node = self.node(dirid)?;
        let Node::Dir {
            parent, children, ..
        } = node
        else {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        };
        match filename.as_ref() {
            b"." => Ok(dirid),
            b".." => Ok(*parent),
            // Children are name-sorted (`sort_children`), so resolve by
            // binary search — lookup is the hot NFS path and directories can
            // be large.
            raw => children
                .binary_search_by(|&child| {
                    let index = usize::try_from(child).expect("vec index");
                    node_name(&self.nodes[index]).as_bytes().cmp(raw)
                })
                .ok()
                .map(|position| children[position])
                .ok_or(nfsstat3::NFS3ERR_NOENT),
        }
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        Ok(self.attr(id, self.node(id)?))
    }

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let Node::File {
            file_index, size, ..
        } = self.node(id)?
        else {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        };
        if offset >= *size {
            return Ok((Vec::new(), true));
        }
        let remaining = *size - offset;
        let len = u64::from(count.min(MAX_READ_LEN)).min(remaining);
        let len = u32::try_from(len).expect("bounded by MAX_READ_LEN");
        let data = self
            .source
            .read(*file_index, offset, len)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "remote read failed");
                nfsstat3::NFS3ERR_IO
            })?;
        // A short return means the producer's file shrank below the snapshot
        // size — report EOF, or the kernel NFS client re-issues the identical
        // READ forever and the reading process hangs.
        let eof = data.len() < usize::try_from(len).expect("u32 fits usize")
            || offset + data.len() as u64 >= *size;
        Ok((data, eof))
    }

    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(
        &self,
        _dirid: fileid3,
        _dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let Node::Dir { children, .. } = self.node(dirid)? else {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        };
        let start = if start_after == 0 {
            0
        } else {
            children
                .iter()
                .position(|&child| child == start_after)
                .ok_or(nfsstat3::NFS3ERR_BAD_COOKIE)?
                + 1
        };
        let mut entries = Vec::new();
        for &child in children.iter().skip(start).take(max_entries) {
            let node = self.node(child)?;
            entries.push(DirEntry {
                fileid: child,
                name: node_name(node).as_bytes().into(),
                attr: self.attr(child, node),
            });
        }
        let end = start + entries.len() >= children.len();
        Ok(ReadDirResult { entries, end })
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readlink(&self, _id: fileid3) -> Result<nfspath3, nfsstat3> {
        // The scan skips symlinks, so no node is ever a link.
        Err(nfsstat3::NFS3ERR_NOENT)
    }
}

#[cfg(test)]
mod tests {
    use super::super::wire::{DirEntry as WireDir, FileEntry, MountManifest};
    use super::{ByteSource, Node, ROOT_ID, RemoteFs, build_tree, node_name};
    use anyhow::Result;
    use async_trait::async_trait;
    use nfsserve::nfs::nfsstat3;
    use nfsserve::vfs::NFSFileSystem;

    /// An in-memory byte source: one buffer per manifest file index.
    struct MemSource {
        bodies: Vec<Vec<u8>>,
    }

    #[async_trait]
    impl ByteSource for MemSource {
        async fn read(&self, index: u32, offset: u64, len: u32) -> Result<Vec<u8>> {
            let body = &self.bodies[usize::try_from(index).unwrap()];
            let start = usize::try_from(offset).unwrap().min(body.len());
            let end = (start + usize::try_from(len).unwrap()).min(body.len());
            Ok(body[start..end].to_vec())
        }
    }

    fn manifest() -> MountManifest {
        MountManifest {
            dirs: vec![WireDir {
                rel_path: "docs".to_owned(),
                mode: 0o700,
                mtime: 50,
            }],
            files: vec![
                FileEntry {
                    rel_path: "README.md".to_owned(),
                    size: 5,
                    mode: 0o644,
                    mtime: 99,
                },
                FileEntry {
                    // The parent dir `src` is never listed — synthesized.
                    rel_path: "src/lib.rs".to_owned(),
                    size: 3,
                    mode: 0o600,
                    mtime: 98,
                },
            ],
        }
    }

    fn fixture() -> RemoteFs<MemSource> {
        let nodes = build_tree(&manifest()).expect("build tree");
        let source = MemSource {
            bodies: vec![b"hello".to_vec(), b"src".to_vec()],
        };
        RemoteFs::new(nodes, source, 501, 20)
    }

    async fn lookup(fs: &RemoteFs<MemSource>, dir: u64, name: &str) -> Result<u64, nfsstat3> {
        fs.lookup(dir, &name.as_bytes().into()).await
    }

    #[tokio::test]
    async fn lookup_getattr_and_dotdot() {
        let fs = fixture();
        let readme = lookup(&fs, ROOT_ID, "README.md").await.expect("lookup");
        let attr = fs.getattr(readme).await.expect("getattr");
        assert_eq!(attr.size, 5);
        assert_eq!(attr.uid, 501);
        assert_eq!(attr.mode, 0o644);
        assert_eq!(attr.mtime.seconds, 99);

        let src = lookup(&fs, ROOT_ID, "src").await.expect("synthesized dir");
        assert_eq!(lookup(&fs, src, "..").await.expect("dotdot"), ROOT_ID);
        assert!(lookup(&fs, ROOT_ID, "missing").await.is_err());
        // Lookup inside a file is NOTDIR.
        assert_eq!(
            lookup(&fs, readme, "x").await.unwrap_err() as u32,
            nfsstat3::NFS3ERR_NOTDIR as u32
        );
    }

    #[tokio::test]
    async fn dir_mode_is_floored_for_traversal() {
        let fs = fixture();
        let docs = lookup(&fs, ROOT_ID, "docs").await.expect("docs");
        let attr = fs.getattr(docs).await.expect("getattr");
        // Producer said 0o700; the consumer floors in 0o555 so the mounting
        // user can always traverse.
        assert_eq!(attr.mode, 0o755);
    }

    #[tokio::test]
    async fn read_ranges_and_eof() {
        let fs = fixture();
        let readme = lookup(&fs, ROOT_ID, "README.md").await.expect("lookup");

        let (head, head_eof) = fs.read(readme, 0, 2).await.expect("read");
        assert_eq!(head, b"he");
        assert!(!head_eof);

        let (tail, tail_eof) = fs.read(readme, 3, 100).await.expect("read tail");
        assert_eq!(tail, b"lo");
        assert!(tail_eof);

        let (past, past_eof) = fs.read(readme, 5, 1).await.expect("read past eof");
        assert!(past.is_empty());
        assert!(past_eof);
    }

    #[tokio::test]
    async fn readdir_paginates_sorted() {
        let fs = fixture();
        let first = fs.readdir(ROOT_ID, 0, 2).await.expect("page 1");
        assert_eq!(first.entries.len(), 2);
        assert!(!first.end);
        let names: Vec<&str> = first
            .entries
            .iter()
            .map(|entry| std::str::from_utf8(entry.name.as_ref()).unwrap())
            .collect();
        assert_eq!(names, vec!["README.md", "docs"], "sorted by name");

        let last_id = first.entries[1].fileid;
        let second = fs.readdir(ROOT_ID, last_id, 10).await.expect("page 2");
        assert_eq!(second.entries.len(), 1);
        assert!(second.end);

        assert!(
            fs.readdir(ROOT_ID, 424_242, 10).await.is_err(),
            "bad cookie"
        );
    }

    #[tokio::test]
    async fn write_family_is_readonly() {
        let fs = fixture();
        let name: nfsserve::nfs::filename3 = "x".as_bytes().into();
        let rofs = nfsstat3::NFS3ERR_ROFS as u32;
        assert_eq!(fs.write(2, 0, b"x").await.unwrap_err() as u32, rofs);
        assert_eq!(
            fs.create(ROOT_ID, &name, nfsserve::nfs::sattr3::default())
                .await
                .unwrap_err() as u32,
            rofs
        );
        assert_eq!(
            fs.create_exclusive(ROOT_ID, &name).await.unwrap_err() as u32,
            rofs
        );
        assert_eq!(fs.mkdir(ROOT_ID, &name).await.unwrap_err() as u32, rofs);
        assert_eq!(fs.remove(ROOT_ID, &name).await.unwrap_err() as u32, rofs);
        assert_eq!(
            fs.rename(ROOT_ID, &name, ROOT_ID, &name).await.unwrap_err() as u32,
            rofs
        );
    }

    #[tokio::test]
    async fn shrunken_producer_file_reports_eof() {
        // The producer's file shrank below the snapshot size after the scan:
        // the source returns short. eof must be true, or the kernel NFS
        // client re-issues the identical READ forever.
        let nodes = build_tree(&manifest()).expect("build tree");
        let source = MemSource {
            // README.md's snapshot size is 5, but only 2 bytes remain.
            bodies: vec![b"he".to_vec(), b"src".to_vec()],
        };
        let fs = RemoteFs::new(nodes, source, 501, 20);
        let readme = lookup(&fs, ROOT_ID, "README.md").await.expect("lookup");
        let (data, eof) = fs.read(readme, 0, 5).await.expect("read");
        assert_eq!(data, b"he");
        assert!(eof, "a short read past the live EOF must report eof");
    }

    #[test]
    fn deep_hostile_path_builds_without_overflowing() {
        // ~30k components once overflowed the stack via recursion; the
        // iterative ensure_dir must just build the (absurd) tree.
        let deep = vec!["d"; 30_000].join("/");
        let manifest = MountManifest {
            dirs: Vec::new(),
            files: vec![FileEntry {
                rel_path: format!("{deep}/leaf"),
                size: 1,
                mode: 0o644,
                mtime: 0,
            }],
        };
        let nodes = build_tree(&manifest).expect("deep tree builds iteratively");
        // Filler slot + root + 30k dirs + the leaf file.
        assert_eq!(nodes.len(), 30_003);
    }

    #[test]
    fn hostile_manifest_paths_are_rejected() {
        for bad in ["../escape", "a/../b", "a//b", "/abs", "nul\0byte"] {
            let manifest = MountManifest {
                dirs: Vec::new(),
                files: vec![FileEntry {
                    rel_path: (*bad).to_owned(),
                    size: 1,
                    mode: 0o644,
                    mtime: 0,
                }],
            };
            assert!(build_tree(&manifest).is_err(), "must reject {bad:?}");
        }
    }

    #[test]
    fn duplicate_and_file_as_dir_are_rejected() {
        let duplicate = MountManifest {
            dirs: Vec::new(),
            files: vec![
                FileEntry {
                    rel_path: "same".to_owned(),
                    size: 1,
                    mode: 0o644,
                    mtime: 0,
                },
                FileEntry {
                    rel_path: "same".to_owned(),
                    size: 2,
                    mode: 0o644,
                    mtime: 0,
                },
            ],
        };
        assert!(build_tree(&duplicate).is_err());

        let file_as_dir = MountManifest {
            dirs: Vec::new(),
            files: vec![
                FileEntry {
                    rel_path: "leaf".to_owned(),
                    size: 1,
                    mode: 0o644,
                    mtime: 0,
                },
                FileEntry {
                    rel_path: "leaf/child".to_owned(),
                    size: 1,
                    mode: 0o644,
                    mtime: 0,
                },
            ],
        };
        assert!(build_tree(&file_as_dir).is_err());
    }

    #[test]
    fn explicit_dir_attrs_survive_synthesis_order() {
        // `docs` appears both as a synthesized parent (via docs/guide.md if
        // files came first) and as an explicit dir — explicit attrs win.
        let nodes = build_tree(&manifest()).expect("build");
        let docs = nodes
            .iter()
            .find(|node| matches!(node, Node::Dir { .. }) && node_name(node) == "docs")
            .expect("docs node");
        if let Node::Dir { mode, mtime, .. } = docs {
            assert_eq!(*mode, 0o700);
            assert_eq!(*mtime, 50);
        }
    }
}
