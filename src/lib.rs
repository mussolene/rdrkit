use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use flate2::read::ZlibDecoder;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, fsinfo3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

mod session;

const MAGIC: u32 = 0xd754_da33;
const FILE_HEADER_SIZE: u64 = 52;
const RAW_DATA_FLAGS: u32 = 0x1800_0020;
const ZLIB_DATA_FLAGS: u32 = 0x1804_0220;
const RAW_CHUNK_INDEX_FLAGS: u32 = 0x0800_0090;
const ZLIB_CHUNK_INDEX_FLAGS: u32 = 0x0804_0290;
const ARCHIVE_DIRECTORY_FLAGS: u32 = 0x0000_0008;
const DIRECTORY_POINTER_FLAGS: u32 = 0x0000_0003;

#[derive(Parser)]
#[command(
    version,
    about = "Read indexed R-Drive Image (.rdr) objects without modifying the source",
    long_about = "rdrkit exposes objects stored in indexed R-Drive Image (.rdr) archives as read-only raw byte streams. It reads the archive footer and compact chunk indexes directly, then inflates only requested chunks.",
    after_help = "Use `rdrkit mount IMAGE.rdr` for the normal read-only workflow. The `list` and `serve` commands remain available for inspection and manual orchestration."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List indexed objects without scanning the complete image.
    List { image: PathBuf },

    /// Scan record headers and report discovered RDR objects.
    Inspect {
        image: PathBuf,

        /// Stop after this many records. Useful for a quick structural probe.
        #[arg(long)]
        max_records: Option<u64>,
    },

    /// Extract one RDR object as a sparse raw image.
    Extract {
        image: PathBuf,

        #[arg(long)]
        object: u32,

        #[arg(long)]
        output: PathBuf,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },

    /// Serve one object as a read-only virtual raw file over localhost NFSv3.
    Serve {
        image: PathBuf,

        #[arg(long)]
        object: u32,

        #[arg(long, default_value = "127.0.0.1:11111")]
        listen: String,

        /// Write the selected listen port after the server is ready.
        #[arg(long, hide = true)]
        ready_file: Option<PathBuf>,
    },

    /// Attach an indexed object and mount its recognized volumes read-only.
    Mount {
        image: PathBuf,

        /// Object to attach. Required non-interactively when the image has several objects.
        #[arg(long)]
        object: Option<u32>,
    },

    /// Detach a mount session by session id or image path.
    Unmount { target: String },

    /// Show known mount sessions.
    Status,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectInfo {
    pub id: u32,
    pub logical_size: u64,
    pub chunks: usize,
    pub chunk_size: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageInfo {
    pub physical_size: u64,
    pub objects: Vec<ObjectInfo>,
}

#[derive(Debug)]
struct FileHeader {
    declared_file_size: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Encoding {
    Raw,
    Zlib,
}

#[derive(Clone, Debug)]
struct DataRecord {
    record_offset: u64,
    total_length: u32,
    payload_offset: u64,
    payload_length: u32,
    logical_offset: u64,
    logical_length: u32,
    encoding: Encoding,
}

#[derive(Debug)]
enum Record {
    Data(DataRecord),
    Metadata {
        record_offset: u64,
        total_length: u32,
        flags: u32,
    },
}

#[derive(Debug, Default)]
struct ObjectStats {
    index: u32,
    records: u64,
    logical_size: u64,
    stored_payload: u64,
    raw_records: u64,
    zlib_records: u64,
    complete: bool,
}

impl ObjectStats {
    fn print(&self) {
        let status = if self.complete { "complete" } else { "partial" };
        println!(
            "object={} status={} logical={} stored={} records={} raw={} zlib={}",
            self.index,
            status,
            human_bytes(self.logical_size),
            human_bytes(self.stored_payload),
            self.records,
            self.raw_records,
            self.zlib_records,
        );
    }
}

pub fn run() -> Result<()> {
    match Cli::parse().command {
        Command::List { image } => list_objects(&image),
        Command::Inspect { image, max_records } => inspect(&image, max_records),
        Command::Extract {
            image,
            object,
            output,
            force,
        } => extract(&image, object, &output, force),
        Command::Serve {
            image,
            object,
            listen,
            ready_file,
        } => serve(&image, object, &listen, ready_file.as_deref()),
        Command::Mount { image, object } => session::mount(&image, object),
        Command::Unmount { target } => session::unmount(&target),
        Command::Status => session::status(),
    }
}

fn list_objects(image: &Path) -> Result<()> {
    let info = image_info(image)?;
    println!(
        "image={} size={}",
        image.display(),
        human_bytes(info.physical_size)
    );
    for object in info.objects {
        println!(
            "object={} size={} chunks={} chunk_size={}",
            object.id,
            human_bytes(object.logical_size),
            object.chunks,
            human_bytes(u64::from(object.chunk_size))
        );
    }
    Ok(())
}

pub fn image_info(image: &Path) -> Result<ImageInfo> {
    let mut input = File::open(image).with_context(|| format!("open {}", image.display()))?;
    let actual_size = input.metadata()?.len();
    let header = read_file_header(&mut input)?;
    ensure!(
        header.declared_file_size == actual_size,
        "RDR header declares {} bytes, file has {} bytes",
        header.declared_file_size,
        actual_size
    );

    let directory = read_archive_directory(&mut input, actual_size)?;
    let mut indexes: Vec<_> = directory
        .iter()
        .filter(|entry| entry.frame_type == 0x90)
        .copied()
        .collect();
    indexes.sort_unstable_by_key(|entry| entry.object_id);
    ensure!(
        !indexes.is_empty(),
        "archive contains no compact chunk indexes"
    );

    let mut objects = Vec::with_capacity(indexes.len());
    for frame in indexes {
        let (logical_size, chunk_size, records) =
            read_chunk_index(&mut input, actual_size, &frame, frame.object_id)?;
        objects.push(ObjectInfo {
            id: frame.object_id,
            logical_size,
            chunks: records.len(),
            chunk_size,
        });
    }
    Ok(ImageInfo {
        physical_size: actual_size,
        objects,
    })
}

const ROOT_FILE_ID: fileid3 = 1;
const RAW_FILE_ID: fileid3 = 2;

struct ReaderState {
    input: File,
    file_size: u64,
    cached_record_offset: Option<u64>,
    cached_data: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct ChunkIndexEntry {
    record_offset: u64,
    total_length: u32,
}

struct IndexedObject {
    filename: String,
    records: Vec<ChunkIndexEntry>,
    logical_size: u64,
    chunk_size: u32,
    state: Mutex<ReaderState>,
}

impl IndexedObject {
    fn attributes(&self, id: fileid3) -> std::result::Result<fattr3, nfsstat3> {
        let common = fattr3 {
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: specdata3::default(),
            fsid: 0x5244_524b_4954,
            fileid: id,
            atime: nfstime3::default(),
            mtime: nfstime3::default(),
            ctime: nfstime3::default(),
            ..fattr3::default()
        };
        match id {
            ROOT_FILE_ID => Ok(fattr3 {
                ftype: ftype3::NF3DIR,
                mode: 0o555,
                nlink: 2,
                ..common
            }),
            RAW_FILE_ID => Ok(fattr3 {
                ftype: ftype3::NF3REG,
                mode: 0o444,
                size: self.logical_size,
                used: self.logical_size,
                ..common
            }),
            _ => Err(nfsstat3::NFS3ERR_NOENT),
        }
    }

    fn read_range(&self, offset: u64, count: u32) -> Result<Vec<u8>> {
        if offset >= self.logical_size || count == 0 {
            return Ok(Vec::new());
        }
        let end = self
            .logical_size
            .min(offset.saturating_add(u64::from(count)));
        let mut result = vec![0_u8; (end - offset) as usize];
        let mut cursor = offset;
        let mut state = self.state.lock().expect("RDR reader mutex poisoned");

        while cursor < end {
            let index = usize::try_from(cursor / u64::from(self.chunk_size))
                .context("chunk index does not fit usize")?;
            let entry = self
                .records
                .get(index)
                .context("chunk index is incomplete")?;
            let logical_offset = index as u64 * u64::from(self.chunk_size);
            let logical_length =
                (self.logical_size - logical_offset).min(u64::from(self.chunk_size)) as u32;
            let record_end = logical_offset + u64::from(logical_length);

            if state.cached_record_offset != Some(entry.record_offset) {
                let file_size = state.file_size;
                let record = match read_record(&mut state.input, entry.record_offset, file_size)? {
                    Record::Data(record) => record,
                    Record::Metadata { .. } => anyhow::bail!(
                        "chunk index points to metadata at 0x{:x}",
                        entry.record_offset
                    ),
                };
                ensure!(
                    record.total_length == entry.total_length
                        && record.logical_offset == logical_offset
                        && record.logical_length == logical_length,
                    "chunk index mismatch at 0x{:x}",
                    entry.record_offset
                );
                state.cached_data = decode_record(&mut state.input, &record)?;
                state.cached_record_offset = Some(entry.record_offset);
            }

            let copy_end = end.min(record_end);
            let source_start = (cursor - logical_offset) as usize;
            let source_end = (copy_end - logical_offset) as usize;
            let target_start = (cursor - offset) as usize;
            let target_end = (copy_end - offset) as usize;
            result[target_start..target_end]
                .copy_from_slice(&state.cached_data[source_start..source_end]);
            cursor = copy_end;
        }
        Ok(result)
    }
}

#[async_trait]
impl NFSFileSystem for IndexedObject {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_FILE_ID
    }

    async fn lookup(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> std::result::Result<fileid3, nfsstat3> {
        if dirid != ROOT_FILE_ID {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        if filename.as_ref() == self.filename.as_bytes() {
            Ok(RAW_FILE_ID)
        } else {
            Err(nfsstat3::NFS3ERR_NOENT)
        }
    }

    async fn getattr(&self, id: fileid3) -> std::result::Result<fattr3, nfsstat3> {
        self.attributes(id)
    }

    async fn setattr(
        &self,
        _id: fileid3,
        _setattr: sattr3,
    ) -> std::result::Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> std::result::Result<(Vec<u8>, bool), nfsstat3> {
        if id == ROOT_FILE_ID {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        if id != RAW_FILE_ID {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }
        let data = self
            .read_range(offset, count)
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let eof = offset.saturating_add(data.len() as u64) >= self.logical_size;
        Ok((data, eof))
    }

    async fn write(
        &self,
        _id: fileid3,
        _offset: u64,
        _data: &[u8],
    ) -> std::result::Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> std::result::Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> std::result::Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(
        &self,
        _dirid: fileid3,
        _dirname: &filename3,
    ) -> std::result::Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> std::result::Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> std::result::Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> std::result::Result<ReadDirResult, nfsstat3> {
        if dirid != ROOT_FILE_ID {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let entries = if start_after < RAW_FILE_ID && max_entries > 0 {
            vec![DirEntry {
                fileid: RAW_FILE_ID,
                name: self.filename.as_bytes().into(),
                attr: self.attributes(RAW_FILE_ID)?,
            }]
        } else {
            Vec::new()
        };
        Ok(ReadDirResult { entries, end: true })
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> std::result::Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readlink(&self, _id: fileid3) -> std::result::Result<nfspath3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_INVAL)
    }

    async fn fsinfo(&self, _root_fileid: fileid3) -> std::result::Result<fsinfo3, nfsstat3> {
        Ok(fsinfo3 {
            obj_attributes: nfsserve::nfs::post_op_attr::attributes(self.attributes(ROOT_FILE_ID)?),
            rtmax: 1024 * 1024,
            rtpref: 128 * 1024,
            rtmult: 4096,
            wtmax: 0,
            wtpref: 0,
            wtmult: 0,
            dtpref: 4096,
            maxfilesize: u64::MAX,
            time_delta: nfstime3 {
                seconds: 0,
                nseconds: 1_000_000,
            },
            properties: nfsserve::nfs::FSF_HOMOGENEOUS,
        })
    }
}

fn serve(image: &Path, object_index: u32, listen: &str, ready_file: Option<&Path>) -> Result<()> {
    let indexed = index_object(image, object_index)?;
    println!(
        "indexed object={} size={} records={} file={}",
        object_index,
        human_bytes(indexed.logical_size),
        indexed.records.len(),
        indexed.filename
    );
    println!("NFS listen={listen} export=/");

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let listener = NFSTcpListener::bind(listen, indexed).await?;
        let listen_port = listener.get_listen_port();
        println!("NFS ready_port={listen_port}");
        if let Some(path) = ready_file {
            write_ready_file(path, listen_port)?;
        }
        listener.handle_forever().await?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

fn write_ready_file(path: &Path, listen_port: u16) -> Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, listen_port.to_string())
        .with_context(|| format!("write readiness file {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("publish readiness file {}", path.display()))?;
    Ok(())
}

fn index_object(image: &Path, object_index: u32) -> Result<IndexedObject> {
    let mut input = File::open(image).with_context(|| format!("open {}", image.display()))?;
    let actual_size = input.metadata()?.len();
    let header = read_file_header(&mut input)?;
    ensure!(
        header.declared_file_size == actual_size,
        "RDR file size mismatch"
    );

    let directory = read_archive_directory(&mut input, actual_size)?;
    let index_frame = directory
        .iter()
        .find(|entry| entry.object_id == object_index && entry.frame_type == 0x90)
        .with_context(|| format!("object {object_index} has no compact chunk index"))?;
    let (logical_size, chunk_size, records) =
        read_chunk_index(&mut input, actual_size, index_frame, object_index)?;

    Ok(IndexedObject {
        filename: format!("object-{object_index}.raw"),
        records,
        logical_size,
        chunk_size,
        state: Mutex::new(ReaderState {
            input,
            file_size: actual_size,
            cached_record_offset: None,
            cached_data: Vec::new(),
        }),
    })
}

#[derive(Clone, Copy, Debug)]
struct ArchiveDirectoryEntry {
    record_offset: u64,
    total_length: u32,
    object_id: u32,
    frame_type: u32,
}

fn read_archive_directory(input: &mut File, file_size: u64) -> Result<Vec<ArchiveDirectoryEntry>> {
    let tail_size = file_size.min(64 * 1024) as usize;
    let tail_offset = file_size - tail_size as u64;
    input.seek(SeekFrom::Start(tail_offset))?;
    let mut tail = vec![0_u8; tail_size];
    input.read_exact(&mut tail)?;

    let pointer_at = (0..=tail.len().saturating_sub(24)).rev().find(|&offset| {
        le_u32(&tail, offset) == MAGIC
            && le_u32(&tail, offset + 4) == 24
            && le_u32(&tail, offset + 8) == DIRECTORY_POINTER_FLAGS
    });
    let pointer_at = pointer_at.context("RDR archive directory pointer was not found in footer")?;
    let directory_offset = FILE_HEADER_SIZE
        .checked_add(le_u64(&tail, pointer_at + 12))
        .context("archive directory offset overflow")?;
    let directory_length = le_u32(&tail, pointer_at + 20);
    ensure!(directory_length >= 12, "invalid archive directory length");
    ensure!(
        directory_offset + u64::from(directory_length) <= file_size,
        "archive directory extends past EOF"
    );

    input.seek(SeekFrom::Start(directory_offset))?;
    let mut bytes = vec![0_u8; directory_length as usize];
    input.read_exact(&mut bytes)?;
    ensure!(le_u32(&bytes, 0) == MAGIC, "bad archive directory magic");
    ensure!(
        le_u32(&bytes, 4) == directory_length,
        "archive directory length mismatch"
    );
    ensure!(
        le_u32(&bytes, 8) == ARCHIVE_DIRECTORY_FLAGS,
        "unsupported archive directory record"
    );
    ensure!((bytes.len() - 12) % 20 == 0, "malformed archive directory");

    let mut entries = Vec::with_capacity((bytes.len() - 12) / 20);
    for entry in bytes[12..].chunks_exact(20) {
        let record_offset = FILE_HEADER_SIZE
            .checked_add(le_u64(entry, 0))
            .context("directory entry offset overflow")?;
        let total_length = le_u32(entry, 8);
        ensure!(
            total_length >= 12 && record_offset + u64::from(total_length) <= file_size,
            "archive directory entry points outside the image"
        );
        entries.push(ArchiveDirectoryEntry {
            record_offset,
            total_length,
            object_id: le_u32(entry, 12),
            frame_type: le_u32(entry, 16),
        });
    }
    Ok(entries)
}

fn read_chunk_index(
    input: &mut File,
    file_size: u64,
    frame: &ArchiveDirectoryEntry,
    object_index: u32,
) -> Result<(u64, u32, Vec<ChunkIndexEntry>)> {
    ensure!(frame.total_length >= 20, "chunk index frame is too short");
    input.seek(SeekFrom::Start(frame.record_offset))?;
    let mut frame_bytes = vec![0_u8; frame.total_length as usize];
    input.read_exact(&mut frame_bytes)?;
    ensure!(le_u32(&frame_bytes, 0) == MAGIC, "bad chunk index magic");
    ensure!(
        le_u32(&frame_bytes, 4) == frame.total_length,
        "chunk index length mismatch"
    );
    let flags = le_u32(&frame_bytes, 8);
    let decoded = match flags {
        RAW_CHUNK_INDEX_FLAGS => {
            ensure!(
                le_u32(&frame_bytes, 12) == object_index,
                "chunk index object id mismatch"
            );
            frame_bytes[20..].to_vec()
        }
        ZLIB_CHUNK_INDEX_FLAGS => {
            ensure!(
                frame_bytes.len() >= 24,
                "compressed chunk index frame is too short"
            );
            ensure!(
                le_u32(&frame_bytes, 16) == object_index,
                "chunk index object id mismatch"
            );
            let decoded_length = le_u32(&frame_bytes, 12) as usize;
            let mut decoder = ZlibDecoder::new(&frame_bytes[24..]);
            let mut decoded = Vec::with_capacity(decoded_length);
            decoder
                .read_to_end(&mut decoded)
                .context("inflate compact chunk index")?;
            ensure!(
                decoded.len() == decoded_length,
                "chunk index decoded length mismatch"
            );
            decoded
        }
        _ => anyhow::bail!("unsupported chunk index encoding 0x{flags:08x}"),
    };
    ensure!(decoded.len() >= 28, "chunk index payload is too short");

    let logical_size = le_u64(&decoded, 0);
    let chunk_size_u64 = le_u64(&decoded, 16);
    let chunk_size = u32::try_from(chunk_size_u64).context("unsupported chunk size")?;
    let chunk_count = le_u32(&decoded, 24) as usize;
    ensure!(
        logical_size > 0 && chunk_size > 0,
        "invalid chunk index geometry"
    );
    ensure!(
        decoded.len() == 28 + chunk_count * 12,
        "malformed compact chunk index"
    );
    ensure!(
        logical_size.div_ceil(u64::from(chunk_size)) == chunk_count as u64,
        "chunk count does not cover the logical object"
    );

    let mut records = Vec::with_capacity(chunk_count);
    let mut previous_offset = None;
    for raw_entry in decoded[28..].chunks_exact(12) {
        let record_offset = FILE_HEADER_SIZE
            .checked_add(le_u64(raw_entry, 0))
            .context("chunk record offset overflow")?;
        let total_length = le_u32(raw_entry, 8);
        ensure!(
            total_length >= 12 && record_offset + u64::from(total_length) <= file_size,
            "chunk index entry points outside the image"
        );
        ensure!(
            previous_offset.is_none_or(|previous| record_offset > previous),
            "chunk index offsets are not strictly increasing"
        );
        previous_offset = Some(record_offset);
        records.push(ChunkIndexEntry {
            record_offset,
            total_length,
        });
    }

    let mut samples = vec![0, records.len() - 1];
    if records.len() > 2 {
        samples.push(1);
        samples.push(records.len() / 2);
    }
    samples.sort_unstable();
    samples.dedup();
    for index in samples {
        let entry = records[index];
        let record = match read_record(input, entry.record_offset, file_size)? {
            Record::Data(record) => record,
            Record::Metadata { .. } => anyhow::bail!(
                "chunk index points to metadata at 0x{:x}",
                entry.record_offset
            ),
        };
        let expected_offset = index as u64 * u64::from(chunk_size);
        let expected_length = (logical_size - expected_offset).min(u64::from(chunk_size)) as u32;
        ensure!(
            record.total_length == entry.total_length
                && record.logical_offset == expected_offset
                && record.logical_length == expected_length,
            "chunk index verification failed at entry {index}"
        );
    }

    Ok((logical_size, chunk_size, records))
}

fn inspect(path: &Path, max_records: Option<u64>) -> Result<()> {
    let mut input = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let actual_size = input.metadata()?.len();
    let header = read_file_header(&mut input)?;
    ensure!(
        header.declared_file_size == actual_size,
        "RDR header declares {} bytes, file has {} bytes",
        header.declared_file_size,
        actual_size
    );

    println!(
        "image={} size={} declared_size={}",
        path.display(),
        human_bytes(actual_size),
        human_bytes(header.declared_file_size)
    );

    let mut offset = FILE_HEADER_SIZE;
    let mut record_count = 0_u64;
    let mut current: Option<ObjectStats> = None;
    let mut next_object_index = 0_u32;
    let mut boundary_seen = false;

    while offset < actual_size {
        if max_records.is_some_and(|limit| record_count >= limit) {
            break;
        }

        let record = read_record(&mut input, offset, actual_size)?;
        record_count += 1;

        match record {
            Record::Data(data) => {
                let starts_new = current.is_none()
                    || (data.logical_offset == 0
                        && current.as_ref().is_some_and(|item| item.records > 0)
                        && boundary_seen);

                if starts_new {
                    if let Some(mut finished) = current.take() {
                        finished.complete = true;
                        finished.print();
                    }
                    current = Some(ObjectStats {
                        index: next_object_index,
                        ..ObjectStats::default()
                    });
                    next_object_index += 1;
                }

                let stats = current.as_mut().context("data record without object")?;
                stats.records += 1;
                stats.logical_size = stats
                    .logical_size
                    .max(data.logical_offset + u64::from(data.logical_length));
                stats.stored_payload += u64::from(data.payload_length);
                match data.encoding {
                    Encoding::Raw => stats.raw_records += 1,
                    Encoding::Zlib => stats.zlib_records += 1,
                }
                boundary_seen = false;
                offset += u64::from(data.total_length);
            }
            Record::Metadata {
                record_offset,
                total_length,
                flags,
            } => {
                if flags != 0x0804_0290 && flags != 0x0804_0293 && flags != 0x0000_0002 {
                    eprintln!(
                        "metadata offset=0x{record_offset:x} length={total_length} flags=0x{flags:08x}"
                    );
                }
                boundary_seen = true;
                offset += u64::from(total_length);
            }
        }
    }

    if let Some(stats) = current {
        stats.print();
    }
    println!("records_scanned={record_count} next_file_offset=0x{offset:x}");
    Ok(())
}

fn extract(image: &Path, object_index: u32, output: &Path, force: bool) -> Result<()> {
    let mut input = File::open(image).with_context(|| format!("open {}", image.display()))?;
    let actual_size = input.metadata()?.len();
    let header = read_file_header(&mut input)?;
    ensure!(
        header.declared_file_size == actual_size,
        "RDR file size mismatch"
    );

    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut out = options
        .open(output)
        .with_context(|| format!("create {}", output.display()))?;

    let mut offset = FILE_HEADER_SIZE;
    let mut current_index: Option<u32> = None;
    let mut next_index = 0_u32;
    let mut boundary_seen = false;
    let mut extracted_records = 0_u64;
    let mut logical_size = 0_u64;

    while offset < actual_size {
        let record = read_record(&mut input, offset, actual_size)?;
        match record {
            Record::Metadata { total_length, .. } => {
                boundary_seen = true;
                offset += u64::from(total_length);
            }
            Record::Data(data) => {
                let starts_new = current_index.is_none()
                    || (data.logical_offset == 0 && current_index.is_some() && boundary_seen);
                if starts_new {
                    if current_index == Some(object_index) {
                        break;
                    }
                    current_index = Some(next_index);
                    next_index += 1;
                }

                if current_index == Some(object_index) {
                    write_data_record(&mut input, &mut out, &data)?;
                    extracted_records += 1;
                    logical_size =
                        logical_size.max(data.logical_offset + u64::from(data.logical_length));
                    if extracted_records % 256 == 0 {
                        eprint!(
                            "\robject={} records={} written={}",
                            object_index,
                            extracted_records,
                            human_bytes(logical_size)
                        );
                        io::stderr().flush()?;
                    }
                }

                boundary_seen = false;
                offset += u64::from(data.total_length);
            }
        }
    }

    ensure!(
        current_index.is_some_and(|index| index >= object_index) && extracted_records > 0,
        "object {object_index} was not found"
    );
    out.set_len(logical_size)?;
    out.sync_all()?;
    eprintln!();
    println!(
        "extracted object={} records={} size={} output={}",
        object_index,
        extracted_records,
        human_bytes(logical_size),
        output.display()
    );
    Ok(())
}

fn write_data_record(input: &mut File, output: &mut File, record: &DataRecord) -> Result<()> {
    let decoded = decode_record(input, record)?;
    output.seek(SeekFrom::Start(record.logical_offset))?;
    output.write_all(&decoded)?;
    Ok(())
}

fn decode_record(input: &mut File, record: &DataRecord) -> Result<Vec<u8>> {
    input.seek(SeekFrom::Start(record.payload_offset))?;
    let mut payload = vec![0_u8; record.payload_length as usize];
    input.read_exact(&mut payload)?;

    match record.encoding {
        Encoding::Raw => {
            ensure!(
                payload.len() == record.logical_length as usize,
                "raw record at 0x{:x}: payload {} != logical {}",
                record.record_offset,
                payload.len(),
                record.logical_length
            );
            Ok(payload)
        }
        Encoding::Zlib => {
            let mut decoder = ZlibDecoder::new(payload.as_slice());
            let mut decoded = Vec::with_capacity(record.logical_length as usize);
            decoder
                .read_to_end(&mut decoded)
                .with_context(|| format!("inflate record at 0x{:x}", record.record_offset))?;
            ensure!(
                decoded.len() == record.logical_length as usize,
                "zlib record at 0x{:x}: decoded {} != logical {}",
                record.record_offset,
                decoded.len(),
                record.logical_length
            );
            Ok(decoded)
        }
    }
}

fn read_file_header(file: &mut File) -> Result<FileHeader> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = [0_u8; FILE_HEADER_SIZE as usize];
    file.read_exact(&mut bytes)
        .context("read RDR file header")?;
    ensure!(le_u32(&bytes, 0) == MAGIC, "not an RDR stream: bad magic");
    ensure!(
        le_u32(&bytes, 4) == FILE_HEADER_SIZE as u32,
        "unsupported RDR file header size {}",
        le_u32(&bytes, 4)
    );
    Ok(FileHeader {
        declared_file_size: le_u64(&bytes, 44),
    })
}

fn read_record(file: &mut File, offset: u64, file_size: u64) -> Result<Record> {
    file.seek(SeekFrom::Start(offset))?;
    let mut common = [0_u8; 12];
    file.read_exact(&mut common)
        .with_context(|| format!("read record header at 0x{offset:x}"))?;

    ensure!(
        le_u32(&common, 0) == MAGIC,
        "bad record magic at 0x{offset:x}"
    );
    let total_length = le_u32(&common, 4);
    let flags = le_u32(&common, 8);
    ensure!(total_length >= 12, "invalid record length at 0x{offset:x}");
    ensure!(
        offset + u64::from(total_length) <= file_size,
        "record at 0x{offset:x} extends past EOF"
    );

    match flags {
        RAW_DATA_FLAGS => {
            const HEADER_SIZE: usize = 36;
            ensure!(total_length >= HEADER_SIZE as u32, "short raw record");
            let mut header = [0_u8; HEADER_SIZE];
            header[..12].copy_from_slice(&common);
            file.read_exact(&mut header[12..])?;
            Ok(Record::Data(DataRecord {
                record_offset: offset,
                total_length,
                payload_offset: offset + HEADER_SIZE as u64,
                payload_length: total_length - HEADER_SIZE as u32,
                logical_offset: le_u64(&header, 20),
                logical_length: le_u32(&header, 28),
                encoding: Encoding::Raw,
            }))
        }
        ZLIB_DATA_FLAGS => {
            const HEADER_SIZE: usize = 40;
            ensure!(total_length >= HEADER_SIZE as u32, "short zlib record");
            let mut header = [0_u8; HEADER_SIZE];
            header[..12].copy_from_slice(&common);
            file.read_exact(&mut header[12..])?;
            let declared_length = le_u32(&header, 12);
            let logical_length = le_u32(&header, 32);
            ensure!(
                declared_length == logical_length,
                "zlib record at 0x{offset:x}: conflicting logical sizes"
            );
            Ok(Record::Data(DataRecord {
                record_offset: offset,
                total_length,
                payload_offset: offset + HEADER_SIZE as u64,
                payload_length: total_length - HEADER_SIZE as u32,
                logical_offset: le_u64(&header, 24),
                logical_length,
                encoding: Encoding::Zlib,
            }))
        }
        _ => Ok(Record::Metadata {
            record_offset: offset,
            total_length,
            flags,
        }),
    }
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 slice"))
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 slice"))
}

fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut scaled = value as f64;
    let mut unit = 0;
    while scaled >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} {}", UNITS[unit])
    } else {
        format!("{scaled:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{Seek, SeekFrom, Write};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use anyhow::Result;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;

    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("rdrkit-{label}-{}-{nonce}.tmp", std::process::id()))
    }

    #[test]
    fn reads_footer_directed_archive_directory() -> Result<()> {
        let path = temp_path("directory");
        let file_size = 64 * 1024_u64;
        let directory_offset = 512_u64;
        let directory_length = 32_u32;
        let pointer_offset = file_size - 128;
        let mut file = File::create(&path)?;
        file.set_len(file_size)?;

        let mut directory = vec![0_u8; directory_length as usize];
        directory[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        directory[4..8].copy_from_slice(&directory_length.to_le_bytes());
        directory[8..12].copy_from_slice(&ARCHIVE_DIRECTORY_FLAGS.to_le_bytes());
        directory[12..20].copy_from_slice(&(1_024_u64 - FILE_HEADER_SIZE).to_le_bytes());
        directory[20..24].copy_from_slice(&548_u32.to_le_bytes());
        directory[24..28].copy_from_slice(&3_u32.to_le_bytes());
        directory[28..32].copy_from_slice(&0x90_u32.to_le_bytes());
        file.seek(SeekFrom::Start(directory_offset))?;
        file.write_all(&directory)?;

        let mut pointer = [0_u8; 24];
        pointer[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        pointer[4..8].copy_from_slice(&24_u32.to_le_bytes());
        pointer[8..12].copy_from_slice(&DIRECTORY_POINTER_FLAGS.to_le_bytes());
        pointer[12..20].copy_from_slice(&(directory_offset - FILE_HEADER_SIZE).to_le_bytes());
        pointer[20..24].copy_from_slice(&directory_length.to_le_bytes());
        file.seek(SeekFrom::Start(pointer_offset))?;
        file.write_all(&pointer)?;
        file.sync_all()?;

        let mut input = File::open(&path)?;
        let entries = read_archive_directory(&mut input, file_size)?;
        fs::remove_file(path)?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].record_offset, 1_024);
        assert_eq!(entries[0].total_length, 548);
        assert_eq!(entries[0].object_id, 3);
        assert_eq!(entries[0].frame_type, 0x90);
        Ok(())
    }

    #[test]
    fn reads_and_verifies_compact_chunk_index() -> Result<()> {
        let path = temp_path("chunk-index");
        let file_size = 4_096_u64;
        let index_offset = 1_024_u64;
        let mut file = File::create(&path)?;
        file.set_len(file_size)?;

        let logical_size = 512_u64;
        let data_length = 36_u32 + logical_size as u32;
        let mut data_record = vec![0_u8; data_length as usize];
        data_record[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        data_record[4..8].copy_from_slice(&data_length.to_le_bytes());
        data_record[8..12].copy_from_slice(&RAW_DATA_FLAGS.to_le_bytes());
        data_record[20..28].copy_from_slice(&0_u64.to_le_bytes());
        data_record[28..32].copy_from_slice(&(logical_size as u32).to_le_bytes());
        data_record[36..].fill(0xa5);
        file.seek(SeekFrom::Start(FILE_HEADER_SIZE))?;
        file.write_all(&data_record)?;

        let mut decoded = vec![0_u8; 40];
        decoded[0..8].copy_from_slice(&logical_size.to_le_bytes());
        decoded[16..24].copy_from_slice(&logical_size.to_le_bytes());
        decoded[24..28].copy_from_slice(&1_u32.to_le_bytes());
        decoded[28..36].copy_from_slice(&0_u64.to_le_bytes());
        decoded[36..40].copy_from_slice(&data_length.to_le_bytes());
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&decoded)?;
        let compressed = encoder.finish()?;

        let index_length = 24_u32 + compressed.len() as u32;
        let mut index_header = [0_u8; 24];
        index_header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        index_header[4..8].copy_from_slice(&index_length.to_le_bytes());
        index_header[8..12].copy_from_slice(&ZLIB_CHUNK_INDEX_FLAGS.to_le_bytes());
        index_header[12..16].copy_from_slice(&(decoded.len() as u32).to_le_bytes());
        index_header[16..20].copy_from_slice(&0_u32.to_le_bytes());
        file.seek(SeekFrom::Start(index_offset))?;
        file.write_all(&index_header)?;
        file.write_all(&compressed)?;
        file.sync_all()?;

        let frame = ArchiveDirectoryEntry {
            record_offset: index_offset,
            total_length: index_length,
            object_id: 0,
            frame_type: 0x90,
        };
        let mut input = File::open(&path)?;
        let (size, chunk_size, records) = read_chunk_index(&mut input, file_size, &frame, 0)?;
        fs::remove_file(path)?;

        assert_eq!(size, logical_size);
        assert_eq!(chunk_size, logical_size as u32);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record_offset, FILE_HEADER_SIZE);
        assert_eq!(records[0].total_length, data_length);
        Ok(())
    }
}
