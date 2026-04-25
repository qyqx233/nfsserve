use std::fmt;
use std::io::SeekFrom;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use dav_server::fs::{
    DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsResult, FsStream,
    OpenOptions, ReadDirMeta,
};
use dav_server::DavHandler;
use futures_util::stream::Stream;

use crate::mirrorfs::MirrorFS;
use nfsserve::nfs::{
    fattr3, fileid3, ftype3, nfsstat3, nfstime3, sattr3, set_atime, set_mtime, set_size3,
};
use nfsserve::vfs::NFSFileSystem;

// ---------------------------------------------------------------------------
// nfsstat3 -> FsError
// ---------------------------------------------------------------------------
fn map_nfs_err(e: nfsstat3) -> FsError {
    match e {
        nfsstat3::NFS3ERR_NOENT => FsError::NotFound,
        nfsstat3::NFS3ERR_EXIST => FsError::Exists,
        nfsstat3::NFS3ERR_ACCES => FsError::Forbidden,
        nfsstat3::NFS3ERR_NOTDIR => FsError::Forbidden,
        nfsstat3::NFS3ERR_ISDIR => FsError::Forbidden,
        nfsstat3::NFS3ERR_IO => FsError::GeneralFailure,
        nfsstat3::NFS3ERR_NOTSUPP => FsError::NotImplemented,
        nfsstat3::NFS3ERR_ROFS => FsError::Forbidden,
        _ => FsError::GeneralFailure,
    }
}

// ---------------------------------------------------------------------------
// Metadata adapter
// ---------------------------------------------------------------------------
#[derive(Clone)]
struct MirrorDavMetaData {
    attr: fattr3,
}

impl fmt::Debug for MirrorDavMetaData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirrorDavMetaData")
            .field("len", &self.attr.size)
            .field("is_dir", &self.is_dir())
            .finish()
    }
}

impl DavMetaData for MirrorDavMetaData {
    fn len(&self) -> u64 {
        self.attr.size
    }

    fn modified(&self) -> FsResult<SystemTime> {
        let secs = self.attr.mtime.seconds as u64;
        let nsecs = self.attr.mtime.nseconds;
        Ok(UNIX_EPOCH + std::time::Duration::new(secs, nsecs))
    }

    fn is_dir(&self) -> bool {
        matches!(self.attr.ftype, ftype3::NF3DIR)
    }

    fn is_file(&self) -> bool {
        matches!(self.attr.ftype, ftype3::NF3REG)
    }

    fn is_symlink(&self) -> bool {
        matches!(self.attr.ftype, ftype3::NF3LNK)
    }

    fn accessed(&self) -> FsResult<SystemTime> {
        let secs = self.attr.atime.seconds as u64;
        let nsecs = self.attr.atime.nseconds;
        Ok(UNIX_EPOCH + std::time::Duration::new(secs, nsecs))
    }

    fn status_changed(&self) -> FsResult<SystemTime> {
        let secs = self.attr.ctime.seconds as u64;
        let nsecs = self.attr.ctime.nseconds;
        Ok(UNIX_EPOCH + std::time::Duration::new(secs, nsecs))
    }

    fn executable(&self) -> FsResult<bool> {
        Ok(self.attr.mode & 0o111 != 0)
    }
}

// ---------------------------------------------------------------------------
// Directory entry adapter
// ---------------------------------------------------------------------------
struct MirrorDavDirEntry {
    name: Vec<u8>,
    attr: fattr3,
}

impl DavDirEntry for MirrorDavDirEntry {
    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }

    fn metadata(&'_ self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = MirrorDavMetaData { attr: self.attr };
        Box::pin(async move { Ok(Box::new(meta) as Box<dyn DavMetaData>) })
    }
}

// ---------------------------------------------------------------------------
// File adapter
// ---------------------------------------------------------------------------
struct MirrorDavFile {
    fs: Arc<MirrorFS>,
    id: fileid3,
    offset: Arc<AtomicU64>,
}

impl fmt::Debug for MirrorDavFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirrorDavFile")
            .field("id", &self.id)
            .field("offset", &self.offset.load(Ordering::Relaxed))
            .finish()
    }
}

impl DavFile for MirrorDavFile {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let fs = self.fs.clone();
        let id = self.id;
        Box::pin(async move {
            let attr = fs.getattr(id).await.map_err(map_nfs_err)?;
            Ok(Box::new(MirrorDavMetaData { attr }) as Box<dyn DavMetaData>)
        })
    }

    fn write_buf(&'_ mut self, mut buf: Box<dyn ::bytes::Buf + Send>) -> FsFuture<'_, ()> {
        let fs = self.fs.clone();
        let id = self.id;
        let offset_arc = self.offset.clone();
        Box::pin(async move {
            let data = buf.copy_to_bytes(buf.remaining());
            let offset = offset_arc.load(Ordering::Relaxed);
            fs.write(id, offset, &data).await.map_err(map_nfs_err)?;
            offset_arc.fetch_add(data.len() as u64, Ordering::Relaxed);
            Ok(())
        })
    }

    fn write_bytes(&'_ mut self, buf: Bytes) -> FsFuture<'_, ()> {
        let fs = self.fs.clone();
        let id = self.id;
        let offset_arc = self.offset.clone();
        Box::pin(async move {
            let offset = offset_arc.load(Ordering::Relaxed);
            fs.write(id, offset, &buf).await.map_err(map_nfs_err)?;
            offset_arc.fetch_add(buf.len() as u64, Ordering::Relaxed);
            Ok(())
        })
    }

    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, Bytes> {
        let fs = self.fs.clone();
        let id = self.id;
        let offset_arc = self.offset.clone();
        Box::pin(async move {
            let offset = offset_arc.load(Ordering::Relaxed);
            let (data, _eof) = fs
                .read(id, offset, count as u32)
                .await
                .map_err(map_nfs_err)?;
            offset_arc.fetch_add(data.len() as u64, Ordering::Relaxed);
            Ok(Bytes::from(data))
        })
    }

    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        let fs = self.fs.clone();
        let id = self.id;
        let offset_arc = self.offset.clone();
        Box::pin(async move {
            let attr = fs.getattr(id).await.map_err(map_nfs_err)?;
            let current = offset_arc.load(Ordering::Relaxed);
            let new_offset = match pos {
                SeekFrom::Start(o) => o,
                SeekFrom::End(o) => {
                    if o < 0 {
                        attr.size.saturating_sub((-o) as u64)
                    } else {
                        attr.size + o as u64
                    }
                }
                SeekFrom::Current(o) => {
                    if o < 0 {
                        current.saturating_sub((-o) as u64)
                    } else {
                        current + o as u64
                    }
                }
            };
            offset_arc.store(new_offset, Ordering::Relaxed);
            Ok(new_offset)
        })
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async move { Ok(()) })
    }
}

// ---------------------------------------------------------------------------
// Stream for directory listing
// ---------------------------------------------------------------------------
struct MirrorDavStream<T> {
    items: Vec<T>,
}

impl<T: Send + Unpin> Stream for MirrorDavStream<T> {
    type Item = FsResult<T>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<FsResult<T>>> {
        let this = self.get_mut();
        if this.items.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Ok(this.items.remove(0))))
        }
    }
}

// ---------------------------------------------------------------------------
// Filesystem adapter
// ---------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct MirrorDavFS {
    inner: Arc<MirrorFS>,
}

impl MirrorDavFS {
    pub fn new(fs: MirrorFS) -> Self {
        MirrorDavFS {
            inner: Arc::new(fs),
        }
    }

    pub fn build_handler(&self) -> DavHandler {
        DavHandler::builder()
            .filesystem(Box::new(self.clone()))
            .locksystem(dav_server::fakels::FakeLs::new())
            .build_handler()
    }
}

impl DavFileSystem for MirrorDavFS {
    fn open<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let id = resolve_path(&fs, &path_bytes).await?;

            // If create_new and exists, fail
            if options.create_new && id.is_some() {
                return Err(FsError::Exists);
            }

            let file_id = if let Some(id) = id {
                id
            } else if options.create || options.create_new {
                let parent_path = parent_path(&path_bytes);
                let filename = filename_from_path(&path_bytes);
                let parent_id = resolve_path(&fs, &parent_path)
                    .await?
                    .ok_or(FsError::NotFound)?;
                let (new_id, _attr) = fs
                    .create(parent_id, &filename.into(), sattr3::default())
                    .await
                    .map_err(map_nfs_err)?;
                new_id
            } else {
                return Err(FsError::NotFound);
            };

            // Verify it's a file (or symlink)
            let attr = fs.getattr(file_id).await.map_err(map_nfs_err)?;
            if !matches!(attr.ftype, ftype3::NF3REG | ftype3::NF3LNK) {
                return Err(FsError::Forbidden);
            }

            // Handle truncate
            if options.truncate && options.write {
                let mut setattr = sattr3::default();
                setattr.size = set_size3::size(0);
                fs.setattr(file_id, setattr).await.map_err(map_nfs_err)?;
            }

            Ok(Box::new(MirrorDavFile {
                fs: fs.clone(),
                id: file_id,
                offset: Arc::new(AtomicU64::new(0)),
            }) as Box<dyn DavFile>)
        })
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let id = resolve_path(&fs, &path_bytes)
                .await?
                .ok_or(FsError::NotFound)?;

            let result = fs.readdir(id, 0, 10000).await.map_err(map_nfs_err)?;

            let items: Vec<Box<dyn DavDirEntry>> = result
                .entries
                .into_iter()
                .map(|e| {
                    Box::new(MirrorDavDirEntry {
                        name: e.name.to_vec(),
                        attr: e.attr,
                    }) as Box<dyn DavDirEntry>
                })
                .collect();

            Ok(Box::pin(MirrorDavStream { items }) as FsStream<Box<dyn DavDirEntry>>)
        })
    }

    fn metadata<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
    ) -> FsFuture<'a, Box<dyn DavMetaData>> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let id = resolve_path(&fs, &path_bytes)
                .await?
                .ok_or(FsError::NotFound)?;
            let attr = fs.getattr(id).await.map_err(map_nfs_err)?;
            Ok(Box::new(MirrorDavMetaData { attr }) as Box<dyn DavMetaData>)
        })
    }

    fn create_dir<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
    ) -> FsFuture<'a, ()> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let parent_path = parent_path(&path_bytes);
            let filename = filename_from_path(&path_bytes);
            let parent_id = resolve_path(&fs, &parent_path)
                .await?
                .ok_or(FsError::NotFound)?;
            fs.mkdir(parent_id, &filename.into())
                .await
                .map_err(map_nfs_err)?;
            Ok(())
        })
    }

    fn remove_dir<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
    ) -> FsFuture<'a, ()> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let parent_path = parent_path(&path_bytes);
            let filename = filename_from_path(&path_bytes);
            let parent_id = resolve_path(&fs, &parent_path)
                .await?
                .ok_or(FsError::NotFound)?;
            fs.remove(parent_id, &filename.into())
                .await
                .map_err(map_nfs_err)?;
            Ok(())
        })
    }

    fn remove_file<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
    ) -> FsFuture<'a, ()> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let parent_path = parent_path(&path_bytes);
            let filename = filename_from_path(&path_bytes);
            let parent_id = resolve_path(&fs, &parent_path)
                .await?
                .ok_or(FsError::NotFound)?;
            fs.remove(parent_id, &filename.into())
                .await
                .map_err(map_nfs_err)?;
            Ok(())
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a dav_server::davpath::DavPath,
        to: &'a dav_server::davpath::DavPath,
    ) -> FsFuture<'a, ()> {
        let fs = self.inner.clone();
        let from_bytes = from.as_bytes().to_vec();
        let to_bytes = to.as_bytes().to_vec();
        Box::pin(async move {
            let from_parent = parent_path(&from_bytes);
            let from_name = filename_from_path(&from_bytes);
            let from_parent_id = resolve_path(&fs, &from_parent)
                .await?
                .ok_or(FsError::NotFound)?;

            let to_parent = parent_path(&to_bytes);
            let to_name = filename_from_path(&to_bytes);
            let to_parent_id = resolve_path(&fs, &to_parent)
                .await?
                .ok_or(FsError::NotFound)?;

            fs.rename(from_parent_id, &from_name.into(), to_parent_id, &to_name.into())
                .await
                .map_err(map_nfs_err)?;
            Ok(())
        })
    }

    fn copy<'a>(
        &'a self,
        from: &'a dav_server::davpath::DavPath,
        to: &'a dav_server::davpath::DavPath,
    ) -> FsFuture<'a, ()> {
        let fs = self.inner.clone();
        let from_bytes = from.as_bytes().to_vec();
        let to_bytes = to.as_bytes().to_vec();
        Box::pin(async move {
            let from_id = resolve_path(&fs, &from_bytes)
                .await?
                .ok_or(FsError::NotFound)?;
            let to_parent = parent_path(&to_bytes);
            let to_name = filename_from_path(&to_bytes);
            let to_parent_id = resolve_path(&fs, &to_parent)
                .await?
                .ok_or(FsError::NotFound)?;

            let (to_id, _attr) = fs
                .create(to_parent_id, &to_name.into(), sattr3::default())
                .await
                .map_err(map_nfs_err)?;

            let mut offset = 0u64;
            loop {
                let (data, eof) = fs
                    .read(from_id, offset, 1024 * 1024)
                    .await
                    .map_err(map_nfs_err)?;
                if data.is_empty() {
                    break;
                }
                fs.write(to_id, offset, &data)
                    .await
                    .map_err(map_nfs_err)?;
                offset += data.len() as u64;
                if eof {
                    break;
                }
            }
            Ok(())
        })
    }

    fn set_modified<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
        tm: SystemTime,
    ) -> FsFuture<'a, ()> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let id = resolve_path(&fs, &path_bytes)
                .await?
                .ok_or(FsError::NotFound)?;
            let mut setattr = sattr3::default();
            if let Ok(d) = tm.duration_since(UNIX_EPOCH) {
                setattr.mtime = set_mtime::SET_TO_CLIENT_TIME(nfstime3 {
                    seconds: d.as_secs() as u32,
                    nseconds: d.subsec_nanos(),
                });
            }
            fs.setattr(id, setattr).await.map_err(map_nfs_err)?;
            Ok(())
        })
    }

    fn set_accessed<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
        tm: SystemTime,
    ) -> FsFuture<'a, ()> {
        let fs = self.inner.clone();
        let path_bytes = path.as_bytes().to_vec();
        Box::pin(async move {
            let id = resolve_path(&fs, &path_bytes)
                .await?
                .ok_or(FsError::NotFound)?;
            let mut setattr = sattr3::default();
            if let Ok(d) = tm.duration_since(UNIX_EPOCH) {
                setattr.atime = set_atime::SET_TO_CLIENT_TIME(nfstime3 {
                    seconds: d.as_secs() as u32,
                    nseconds: d.subsec_nanos(),
                });
            }
            fs.setattr(id, setattr).await.map_err(map_nfs_err)?;
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a dav path to a fileid. Returns `Ok(None)` if the path does not exist.
async fn resolve_path(fs: &Arc<MirrorFS>, path: &[u8]) -> FsResult<Option<fileid3>> {
    if path == b"/" || path.is_empty() {
        return Ok(Some(fs.root_dir()));
    }
    match fs.path_to_id(path).await {
        Ok(id) => Ok(Some(id)),
        Err(nfsstat3::NFS3ERR_NOENT) => Ok(None),
        Err(e) => Err(map_nfs_err(e)),
    }
}

fn parent_path(path: &[u8]) -> Vec<u8> {
    if path == b"/" {
        return b"/".to_vec();
    }
    let mut p = path.to_vec();
    while p.ends_with(b"/") {
        p.pop();
    }
    if let Some(pos) = p.iter().rposition(|&c| c == b'/') {
        if pos == 0 {
            b"/".to_vec()
        } else {
            p[..pos].to_vec()
        }
    } else {
        b"/".to_vec()
    }
}

fn filename_from_path(path: &[u8]) -> Vec<u8> {
    let mut p = path.to_vec();
    while p.ends_with(b"/") {
        p.pop();
    }
    if let Some(pos) = p.iter().rposition(|&c| c == b'/') {
        p[pos + 1..].to_vec()
    } else {
        p
    }
}
