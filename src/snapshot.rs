use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, Write},
    ops::Range,
    path::{Path, PathBuf},
    str::FromStr,
    sync::LazyLock,
};

use apply::Apply;
use bstr::{ByteSlice, ByteVec};
use serde::{Deserialize, Serialize};
use zip::{ZipArchive, ZipWriter};

use crate::{CACHE_DIR, DATA_DIR, files, library};

type Timestamp = i64;

pub static SNAPSHOTS_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = DATA_DIR.join("snapshots");
    std::fs::create_dir_all(&dir).expect("data directory should be writable");
    dir
});
pub static AUTOMATIC_SNAPSHOTS_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = SNAPSHOTS_DIR.join("automatic");
    std::fs::create_dir_all(&dir).expect("data directory should be writable");
    dir
});

const COMPRESSION_METHOD: zip::CompressionMethod = zip::CompressionMethod::ZSTD;
static ZIP_OPTIONS: LazyLock<zip::write::SimpleFileOptions> = LazyLock::new(|| {
    zip::write::SimpleFileOptions::default().compression_method(COMPRESSION_METHOD)
});

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SnapshotType {
    Basic,
    Incremental,
    Invalid,
}

impl From<&str> for SnapshotType {
    fn from(value: &str) -> Self {
        match value {
            "Basic" => Self::Basic,
            "Incremental" => Self::Incremental,
            _ => Self::Invalid,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub original_date: chrono::DateTime<chrono::Utc>,
    pub snapshot_type: SnapshotType,
    pub comment: Option<String>,
    pub app_version: String,
}

impl SnapshotMetadata {
    fn new(snapshot_type: SnapshotType) -> Self {
        Self {
            original_date: chrono::Utc::now(),
            snapshot_type,
            comment: None,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

fn read_os_str(name: &std::ffi::OsStr) -> Result<String, std::io::Error> {
    name.to_str().map(str::to_owned).ok_or(std::io::Error::new(
        std::io::ErrorKind::InvalidFilename,
        "file paths must be valid UTF-8",
    ))
}

/// Writes all files and subdirs in `source` to `path` within `writer`
fn archive_write_dir<W>(
    writer: &mut ZipWriter<W>,
    path: &str,
    source: &Path,
    ignored: &HashSet<PathBuf>,
) -> std::io::Result<()>
where
    W: Write + Seek,
{
    if !path.is_empty() {
        writer.add_directory(path, *ZIP_OPTIONS)?;
    }
    let rel_root = PathBuf::from(path);

    for entry in walkdir::WalkDir::new(source)
        .into_iter()
        .filter_entry(|entry| !ignored.contains(entry.path()))
    {
        let entry = entry?;
        if entry.path_is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "symlink directories are unsupported in snapshots",
            ));
        }

        let path = entry.path();
        let rel_name = read_os_str(
            rel_root
                .join(
                    path.strip_prefix(source)
                        .expect("subdirs should always contain source"),
                )
                .as_os_str(),
        )?;

        if entry.file_type().is_file() {
            let mut file = File::open(path)?;
            writer.start_file(rel_name, *ZIP_OPTIONS)?;
            std::io::copy(&mut file, writer)?;
        } else if path != source {
            writer.add_directory(rel_name, *ZIP_OPTIONS)?;
        }
    }

    Ok(())
}

/// Basic snapshotting implementation that directly stores
/// the directory into a zstandard compressed zip archive.
#[derive(Debug, Default)]
pub struct BasicSnapshotBuilder {
    pub root: PathBuf,
    pub destination: PathBuf,
    pub ignored: HashSet<PathBuf>,
    pub comment: Option<String>,
}

impl BasicSnapshotBuilder {
    pub fn new(root: PathBuf, destination: PathBuf) -> Self {
        Self {
            root,
            destination,
            ..Default::default()
        }
    }

    pub fn ignore<P: Into<PathBuf>>(mut self, ignored: impl IntoIterator<Item = P>) -> Self {
        self.ignored.extend(ignored.into_iter().map(|p| p.into()));
        self
    }

    pub fn comment(mut self, comment: String) -> Self {
        self.comment = Some(comment);
        self
    }

    pub fn finalize(&self) -> std::io::Result<String> {
        let BasicSnapshotBuilder {
            destination,
            ignored,
            ..
        } = self;

        let root = self.root.canonicalize()?;
        let dest_name = read_os_str(destination.file_name().ok_or(std::io::Error::new(
            std::io::ErrorKind::InvalidFilename,
            "file name cannot be '..'",
        ))?)?;

        let dest_file = File::create_new(destination)?;
        let mut writer = ZipWriter::new(dest_file);
        writer.set_comment(serde_json::to_string(&SnapshotMetadata {
            comment: self.comment.clone(),
            ..SnapshotMetadata::new(SnapshotType::Basic)
        })?);

        archive_write_dir(&mut writer, "", &root, ignored)?;

        Ok(dest_name)
    }
}

#[derive(Debug, Default)]
pub struct IncrementalSnapshotBuilder {
    root: PathBuf,
    destination: PathBuf,
    ignored: HashSet<PathBuf>,
    comment: Option<String>,
}

impl IncrementalSnapshotBuilder {
    pub fn new(root: PathBuf, destination: PathBuf) -> Self {
        Self {
            root,
            destination,
            ..Default::default()
        }
    }

    pub fn ignore<P: Into<PathBuf>>(mut self, ignored: impl IntoIterator<Item = P>) -> Self {
        self.ignored.extend(ignored.into_iter().map(|p| p.into()));
        self
    }

    pub fn comment(mut self, comment: String) -> Self {
        self.comment = Some(comment);
        self
    }

    fn extend_existing(&self) -> std::io::Result<String> {
        let dest_name = read_os_str(self.destination.file_name().ok_or(std::io::Error::new(
            std::io::ErrorKind::InvalidFilename,
            "file name cannot be '..'",
        ))?)?;

        let existing = File::open(&self.destination)?;
        let archive = ZipArchive::new(existing)?;
        let mut metadata: SnapshotMetadata = serde_json::from_slice(archive.comment())?;

        if metadata.snapshot_type != SnapshotType::Incremental {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "inputted archive is not an incremental archive",
            ));
        }

        metadata.comment = self.comment.clone();

        let rename = read_os_str(self.root.file_name().expect("file name should not be .."))?;
        let hash = files::gen_hash(self.root.as_os_str().as_encoded_bytes());
        let working_dir = CACHE_DIR.join(format!("LXCOMM_TEMP_EXTEND_INCR_{hash}"));
        if !working_dir.try_exists()? {
            std::fs::create_dir_all(&working_dir)?;
        }

        load_incremental_snapshot(rename.clone(), &working_dir, archive, None)?;

        let timestamp = chrono::Utc::now().timestamp();
        let ops = generate_ops_dir(&working_dir.join(&rename), &self.root)?;
        let existing = File::options()
            .read(true)
            .write(true)
            .open(&self.destination)?;
        let mut archive = ZipWriter::new_append(existing)?;
        archive.set_comment(serde_json::to_string(&metadata)?);

        if !ops.is_empty() {
            archive.start_file(timestamp.to_string(), *ZIP_OPTIONS)?;
            for op in ops {
                encode_op(&op, &mut archive)?;
            }
        }

        archive.finish()?;

        std::fs::remove_dir_all(working_dir)?;

        Ok(dest_name)
    }

    fn create_new(&self) -> std::io::Result<String> {
        let root = self.root.canonicalize()?;

        let dest_name = read_os_str(self.destination.file_name().ok_or(std::io::Error::new(
            std::io::ErrorKind::InvalidFilename,
            "file name cannot be '..'",
        ))?)?;

        let writer = File::create_new(&self.destination)?;
        let mut archive = ZipWriter::new(writer);
        archive.set_comment(serde_json::to_string(&SnapshotMetadata {
            comment: self.comment.clone(),
            ..SnapshotMetadata::new(SnapshotType::Incremental)
        })?);

        archive_write_dir(&mut archive, "root", &root, &self.ignored)?;

        Ok(dest_name)
    }

    pub fn finalize(&self) -> std::io::Result<String> {
        if !self.destination.try_exists()? {
            self.create_new()
        } else {
            self.extend_existing()
        }
    }
}

pub fn list_incremental_snapshots(
    archive: &mut ZipArchive<File>,
) -> std::io::Result<BTreeSet<Timestamp>> {
    let mut timestamps = BTreeSet::new();
    for name in archive
        .file_names()
        .filter(|&name| name != "root" && !name.contains(['\\', '/']))
    {
        timestamps.insert(
            name.parse()
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?,
        );
    }

    Ok(timestamps)
}

mod incremental {
    pub(super) const DELETE_START: &str = "I_LXD-";
    pub(super) const CREATE_START: &str = "I_LXC+";
    pub(super) const MODIFY_START: &str = "I_LXM@";

    pub(super) const PATH_START: &str = "I_LXPS__..";
    pub(super) const PATH_END: &str = "..I_LXPE__";

    pub(super) const BYTES_START: &str = "I_LXBS__";
    pub(super) const BYTES_END: &str = "I_LXBE__";

    pub(super) const MARK_DIR: &str = "I_LXMD__";

    // General "grammar" for reading/writing encoded ops
    // DELETE_START ~ PATH_START ~ .+ ~ PATH_END
    // CREATE_START ~ PATH_START ~ .+ ~ PATH_END ~ MARK_DIR
    // CREATE_START ~ PATH_START ~ .+ ~ PATH_END ~ BYTES_START ~ .* ~ BYTES_END
    // MODIFY_START ~ PATH_START ~ .+ ~ PATH_END ~ ":" ~ (0-padded u64) ~ "," ~ (0-padded u64) ":" ~ (0-padded u64) ~ "," ~ (0-padded u64) ~ ":" ~ BYTES_START ~ .* ~ BYTES_END
    // Operations should always be separated by one newline
}

/// The maximum digits that a `u64` can contain in decimal.
const U64_WIDTH: usize = 20;

#[derive(Debug, PartialEq)]
enum SnapshotOp {
    Delete(String),
    CreateDir(String),
    Create(String, Vec<u8>),
    Modify(String, Range<u64>, Range<u64>, Vec<u8>),
}

impl SnapshotOp {
    fn get_name(&self) -> &str {
        match self {
            Self::Delete(s)
            | Self::CreateDir(s)
            | Self::Create(s, _)
            | Self::Modify(s, _, _, _) => s.as_str(),
        }
    }
}

fn generate_ops(
    name: String,
    source_bytes: Option<&[u8]>,
    new_bytes: Option<&[u8]>,
) -> Vec<SnapshotOp> {
    match (source_bytes, new_bytes) {
        (Some(source), Some(new)) => {
            let diffs = similar::capture_diff_slices(similar::Algorithm::Myers, source, new);
            diffs
                .iter()
                .filter_map(|op| {
                    if op.tag() == similar::DiffTag::Equal {
                        return None;
                    }

                    let Range {
                        start: source_start,
                        end: source_end,
                    } = op.old_range();
                    let Range {
                        start: new_start,
                        end: new_end,
                    } = op.new_range();
                    Some(SnapshotOp::Modify(
                        name.clone(),
                        Range {
                            start: source_start as u64,
                            end: source_end as u64,
                        },
                        Range {
                            start: new_start as u64,
                            end: new_end as u64,
                        },
                        new[op.new_range()].to_owned(),
                    ))
                })
                .collect()
        }
        (Some(_), None) => vec![SnapshotOp::Delete(name)],
        (None, Some(new)) => vec![SnapshotOp::Create(name, new.to_owned())],
        (None, None) => Vec::new(),
    }
}

fn generate_ops_dir(source: &Path, new: &Path) -> std::io::Result<Vec<SnapshotOp>> {
    let mut ops = Vec::new();

    for source_entry in walkdir::WalkDir::new(source).contents_first(true) {
        let source_entry = source_entry?;

        let abs = source_entry.path();
        let rel = abs
            .strip_prefix(source)
            .expect("root should be a valid prefix")
            .as_os_str()
            .apply(read_os_str)?;

        let new_path = new.join(&rel);
        if new_path.try_exists()? {
            match (
                source_entry.metadata()?.is_file(),
                new_path.metadata()?.is_file(),
            ) {
                (true, true) => {
                    let source_contents = std::fs::read(abs)?;
                    let new_contents = std::fs::read(&new_path)?;
                    ops.extend(generate_ops(
                        rel,
                        Some(&source_contents),
                        Some(&new_contents),
                    ));
                }
                (true, false) => {
                    ops.push(SnapshotOp::Delete(rel.clone()));
                    ops.push(SnapshotOp::CreateDir(rel));
                }
                (false, true) => {
                    let new_contents = std::fs::read(&new_path)?;
                    ops.push(SnapshotOp::Delete(rel.clone()));
                    ops.push(SnapshotOp::Create(rel, new_contents));
                }
                (false, false) => (),
            }
        } else {
            ops.push(SnapshotOp::Delete(rel));
        }
    }

    for new_entry in walkdir::WalkDir::new(new).contents_first(true) {
        let new_entry = new_entry?;

        let abs = new_entry.path();
        let rel = abs
            .strip_prefix(new)
            .expect("root should be a valid prefix")
            .as_os_str()
            .apply(read_os_str)?;

        let source_path = source.join(&rel);
        if !source_path.try_exists()? {
            if new_entry.file_type().is_file() {
                let contents = std::fs::read(abs)?;
                ops.push(SnapshotOp::Create(rel, contents));
            } else {
                ops.push(SnapshotOp::CreateDir(rel));
            }
        }
    }

    Ok(ops)
}

fn encode_op<W>(op: &SnapshotOp, writer: &mut W) -> std::io::Result<()>
where
    W: Write,
{
    use incremental::*;

    match op {
        SnapshotOp::Delete(path) => {
            writeln!(writer, "{DELETE_START}{PATH_START}{path}{PATH_END}")?;
        }
        SnapshotOp::CreateDir(path) => {
            writeln!(
                writer,
                "{CREATE_START}{PATH_START}{path}{PATH_END}{MARK_DIR}"
            )?;
        }
        SnapshotOp::Create(path, contents) => {
            write!(
                writer,
                "{CREATE_START}{PATH_START}{path}{PATH_END}{BYTES_START}"
            )?;
            writer.write_all(contents)?;
            writeln!(writer, "{BYTES_END}")?;
        }
        SnapshotOp::Modify(path, source_range, new_range, contents) => {
            let (source_start, source_end) = (source_range.start, source_range.end);
            let (new_start, new_end) = (new_range.start, new_range.end);
            write!(
                writer,
                "{MODIFY_START}{PATH_START}{path}{PATH_END}:{source_start:0>U64_WIDTH$},{source_end:0>U64_WIDTH$}:{new_start:0>U64_WIDTH$},{new_end:0>U64_WIDTH$}:{BYTES_START}"
            )?;
            writer.write_all(contents)?;
            writeln!(writer, "{BYTES_END}")?;
        }
    }

    Ok(())
}

fn decode_ops<R>(reader: &mut R) -> std::io::Result<Vec<SnapshotOp>>
where
    R: Read,
{
    use incremental::*;
    use std::io::{Error, ErrorKind};
    macro_rules! ensure {
        ($check:expr, $message:expr) => {
            if !($check) {
                return Err(Error::new(ErrorKind::InvalidData, $message));
            }
        };
    }

    trait TryAsStr {
        fn try_as_str(&self) -> std::io::Result<&str>;
        fn try_parse_as_str<T: FromStr>(&self) -> std::io::Result<T>
        where
            T::Err: Into<Box<dyn std::error::Error + Send + Sync>>;
    }

    impl TryAsStr for [u8] {
        fn try_as_str(&self) -> std::io::Result<&str> {
            str::from_utf8(self).map_err(|err| Error::new(ErrorKind::InvalidData, err))
        }

        fn try_parse_as_str<T: FromStr>(&self) -> std::io::Result<T>
        where
            T::Err: Into<Box<dyn std::error::Error + Send + Sync>>,
        {
            str::from_utf8(self)
                .map_err(|err| Error::new(ErrorKind::InvalidData, err))
                .and_then(|s| {
                    s.parse::<T>()
                        .map_err(|err| Error::new(ErrorKind::InvalidData, err))
                })
        }
    }

    let mut reader = BufReader::new(reader);

    let mut ops = Vec::new();

    let mut method_start = [0; DELETE_START.len()];
    let mut path_marker = [0; PATH_START.len()];
    let mut create_marker = [0; BYTES_START.len()];
    let mut u64_holder = [0; U64_WIDTH];
    let mut scratch = Vec::with_capacity(1024);

    macro_rules! match_exact {
        ($buffer:expr, $pattern:expr, $message:expr) => {
            reader.read_exact(&mut $buffer)?;
            ensure!($buffer.try_as_str()? == $pattern, $message);
        };

        ($byte:literal) => {
            ensure!(
                reader.skip_until($byte)? == 1,
                format!("expected byte {:?}", $byte)
            );
        };
    }

    macro_rules! get_path {
        () => {{
            match_exact!(path_marker, PATH_START, "expected path start marker");
            read_until_match(&mut scratch, &mut reader, PATH_END.as_bytes())?;
            let path = scratch
                .strip_suffix(PATH_END.as_bytes())
                .expect("suffix should exist if pattern successfully matched")
                .try_as_str()?;
            path.to_string()
        }};
    }

    loop {
        if let Err(err) = reader.read_exact(&mut method_start) {
            if err.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            } else {
                return Err(err);
            }
        }

        match method_start.try_as_str()? {
            DELETE_START => {
                ops.push(SnapshotOp::Delete(get_path!()));
            }
            CREATE_START => {
                let path = get_path!();

                reader.read_exact(&mut create_marker)?;
                match create_marker.try_as_str()? {
                    MARK_DIR => ops.push(SnapshotOp::CreateDir(path)),
                    BYTES_START => {
                        read_until_match(&mut scratch, &mut reader, BYTES_END.as_bytes())?;
                        ops.push(SnapshotOp::Create(
                            path,
                            Vec::from_iter(scratch.drain(..scratch.len() - BYTES_END.len())),
                        ));
                    }
                    bytes => {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "expected one of {MARK_DIR} or {BYTES_START}, received {bytes}"
                            ),
                        ));
                    }
                }
            }
            MODIFY_START => {
                let path = get_path!();

                match_exact!(b':');
                reader.read_exact(&mut u64_holder)?;
                let source_start: u64 = u64_holder.try_parse_as_str()?;
                match_exact!(b',');
                reader.read_exact(&mut u64_holder)?;
                let source_end: u64 = u64_holder.try_parse_as_str()?;
                match_exact!(b':');
                reader.read_exact(&mut u64_holder)?;
                let new_start: u64 = u64_holder.try_parse_as_str()?;
                match_exact!(b',');
                reader.read_exact(&mut u64_holder)?;
                let new_end: u64 = u64_holder.try_parse_as_str()?;
                match_exact!(b':');

                match_exact!(create_marker, BYTES_START, "expected bytes start marker");
                read_until_match(&mut scratch, &mut reader, BYTES_END.as_bytes())?;

                ops.push(SnapshotOp::Modify(
                    path,
                    Range {
                        start: source_start,
                        end: source_end,
                    },
                    Range {
                        start: new_start,
                        end: new_end,
                    },
                    Vec::from_iter(scratch.drain(..scratch.len() - BYTES_END.len())),
                ));
            }
            unexpected => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "expected one of {DELETE_START}, {CREATE_START}, or {MODIFY_START}, received {unexpected}"
                    ),
                ));
            }
        }

        match reader.skip_until(b'\n') {
            Ok(len) if len > 1 => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "unexpectedly skipped bytes to next newline",
                ));
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(err) => return Err(err),
            Ok(_) => {}
        }
    }

    Ok(ops)
}

type WorkingCache = HashMap<String, Vec<u8>>;

fn apply_ops(root: &Path, ops: &[SnapshotOp], cache: &mut WorkingCache) -> std::io::Result<()> {
    let root = root.canonicalize()?;

    // Potential change - only encode path once and assume following entries operate on the given path
    let mut modified = HashSet::new();
    for grouped_op in ops.chunk_by(|a, b| a.get_name() == b.get_name()) {
        let name = grouped_op[0].get_name();
        let file_path = root.join(name);
        if let Some(parent) = file_path.parent()
            && parent != root
        {
            std::fs::create_dir_all(parent)?;
        }

        let mut offset = 0;
        for op in grouped_op {
            match op {
                SnapshotOp::Delete(_) => {
                    if file_path.metadata()?.is_file() {
                        std::fs::remove_file(&file_path)?;
                    } else {
                        std::fs::remove_dir_all(&file_path)?;
                    }
                    cache.remove(name);
                }
                SnapshotOp::CreateDir(path) => {
                    if !root.join(path).exists() {
                        std::fs::create_dir(root.join(path))?;
                    }
                }
                SnapshotOp::Create(_, contents) => {
                    cache.insert(name.to_owned(), contents.to_owned());
                    modified.insert(name);
                }
                SnapshotOp::Modify(_, source_range, new_range, contents) => {
                    let mut editing = if let Some(cached) = cache.remove(name) {
                        cached
                    } else {
                        std::fs::read(&file_path)?
                    };
                    let range = source_range.start.wrapping_add(offset) as usize
                        ..source_range.end.wrapping_add(offset) as usize;
                    editing.replace_range(range, contents);

                    let source_len = source_range.end - source_range.start;
                    let new_len = new_range.end - new_range.start;
                    offset = offset.wrapping_add(new_len.wrapping_sub(source_len));
                    cache.insert(name.to_owned(), editing);
                    modified.insert(name);
                }
            }
        }
    }

    for name in modified {
        std::fs::write(root.join(name), &cache[name])?;
    }

    Ok(())
}

#[test]
fn incr_ops_modify() -> eyre::Result<()> {
    let file_name = "myfile.txt";
    let base = "my cool file (まじで)";
    let new = "get it twisted, this is my cooler file";
    let expected_changes = 3;

    let ops = generate_ops(
        file_name.to_owned(),
        Some(base.as_bytes()),
        Some(new.as_bytes()),
    );
    println!("{ops:#?}");
    assert_eq!(ops.len(), expected_changes);

    let mut encoded_ops = Vec::new();
    let mut writer = std::io::Cursor::new(&mut encoded_ops);
    for op in ops.iter() {
        encode_op(op, &mut writer)?;
    }

    dbg!(encoded_ops.as_bstr());

    let mut reader = std::io::Cursor::new(&encoded_ops);
    let decoded_ops = decode_ops(&mut reader)?;

    assert_eq!(ops, decoded_ops);

    let mut editing = base.as_bytes().to_vec();
    let mut offset = 0;
    for op in decoded_ops {
        match op {
            SnapshotOp::Delete(name) => println!("Would delete {name}"),
            SnapshotOp::CreateDir(name) => println!("Would create directory {name}"),
            SnapshotOp::Create(name, contents) => {
                println!("Would create {name} with contents {}", contents.as_bstr())
            }
            SnapshotOp::Modify(name, source_range, new_range, contents) => {
                assert_eq!(name, file_name);
                editing.replace_range(
                    source_range.start.wrapping_add(offset) as usize
                        ..source_range.end.wrapping_add(offset) as usize,
                    &contents,
                );

                let source_len = source_range.end - source_range.start;
                let new_len = new_range.end - new_range.start;
                offset = offset.wrapping_add(new_len.wrapping_sub(source_len));
            }
        }
    }

    assert_eq!(str::from_utf8(&editing)?, new);

    Ok(())
}

#[test]
fn incr_ops_basic() -> eyre::Result<()> {
    let working_directory = CACHE_DIR.join("LXCOMM_TEST_INCREMENTAL");
    if working_directory.try_exists()? {
        std::fs::remove_dir_all(&working_directory)?;
    }
    let test_dir = PathBuf::from("test/IncrementalSnapshotTest");

    let base_dir = test_dir.join("Base").canonicalize()?;
    let revision_dir = test_dir.join("Revision1").canonicalize()?;
    let final_dir = test_dir.join("Final").canonicalize()?;

    let revision_ops = generate_ops_dir(&base_dir, &revision_dir)?;
    let revision_encoded = {
        let mut buffer = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buffer);
        for op in revision_ops {
            encode_op(&op, &mut cursor)?;
        }
        buffer
    };

    let final_ops = generate_ops_dir(&revision_dir, &final_dir)?;
    let final_encoded = {
        let mut buffer = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buffer);
        for op in final_ops {
            encode_op(&op, &mut cursor)?;
        }
        buffer
    };

    let work_dir = working_directory.join("Base");
    dircpy::copy_dir(base_dir, &work_dir)?;
    let mut work_cache = WorkingCache::new();
    let revision_ops = decode_ops(&mut std::io::Cursor::new(&revision_encoded))?;
    apply_ops(&work_dir, &revision_ops, &mut work_cache)?;
    assert!(files::dir_exact_eq(&work_dir, &revision_dir)?);

    let final_ops = decode_ops(&mut std::io::Cursor::new(&final_encoded))?;
    apply_ops(&work_dir, &final_ops, &mut work_cache)?;
    assert!(files::dir_exact_eq(&work_dir, &final_dir)?);

    Ok(())
}

/// Clears `buffer` and reads from `reader` into `buffer` until and including when `pattern` is found.
/// Errors if `pattern` is empty, `pattern` is not found or some other read error occurs.
fn read_until_match<R>(
    buffer: &mut Vec<u8>,
    reader: &mut BufReader<R>,
    pattern: &[u8],
) -> std::io::Result<()>
where
    R: Read,
{
    use std::io::{Error, ErrorKind};

    buffer.clear();

    if pattern.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "pattern was empty"));
    }
    if pattern.len() == 1 {
        return reader.read_until(pattern[0], buffer).map(|_| ());
    }

    let mut window_start = 0;

    while let read = reader.fill_buf()?
        && !read.is_empty()
    {
        let read_len = read.len();
        buffer.extend_from_slice(read);
        while let Some(bytes) = buffer.get(window_start..window_start + pattern.len()) {
            let mut bytes_iter = bytes.iter().enumerate();
            if let Some(match_start) =
                bytes_iter.find_map(|(i, byte)| (*byte == pattern[0]).then_some(i))
            {
                if match_start > 0 {
                    window_start += match_start;
                    continue;
                }

                let match_end = bytes_iter
                    .zip(pattern[1..].iter())
                    .take_while(|((_i, byte), p_byte)| byte == p_byte)
                    .last()
                    .map(|((i, _b), _p_b)| i);

                if let Some(match_end) = match_end {
                    if match_end == (pattern.len() - 1) {
                        let end_index = window_start + match_end;
                        debug_assert!(&buffer[window_start..=end_index] == pattern);
                        if end_index <= buffer.len() {
                            reader.consume(read_len + end_index - buffer.len() + 1);
                            buffer.drain(window_start + match_end + 1..);
                        } else {
                            reader.consume(read_len);
                        }
                        return Ok(());
                    } else {
                        window_start += match_end
                    }
                } else {
                    window_start += 1;
                }
            } else {
                window_start += pattern.len();
            }
        }
        reader.consume(read_len);
    }
    Err(Error::new(
        ErrorKind::UnexpectedEof,
        format!("pattern {:?} not found in reader", pattern.as_bstr()),
    ))
}

#[test]
fn test_read_until_match() -> eyre::Result<()> {
    let my_str = "this cool thing is my cool str, get it twisted";
    let my_pattern = b"cool str";
    let mut my_buffer = Vec::new();
    let mut buf_reader = BufReader::new(my_str.as_bytes());

    read_until_match(&mut my_buffer, &mut buf_reader, b"")
        .expect_err("empty patterns should be invalid");

    read_until_match(&mut my_buffer, &mut buf_reader, my_pattern)?;
    assert_eq!(&my_buffer, b"this cool thing is my cool str");
    my_buffer.clear();
    buf_reader.read_to_end(&mut my_buffer)?;
    assert_eq!(&my_buffer, b", get it twisted");

    Ok(())
}

pub fn load_basic_snapshot(
    rename: String,
    destination: &Path,
    mut archive: ZipArchive<File>,
) -> std::io::Result<library::Profile> {
    let destination = destination.join(&rename);

    let metadata: SnapshotMetadata = serde_json::from_slice(archive.comment())?;
    if metadata.snapshot_type != SnapshotType::Basic {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "inputted archive was not a basic snapshot",
        ));
    }

    let hash = files::gen_hash(destination.as_os_str().as_encoded_bytes());
    let temp_dir = CACHE_DIR.join(format!("LXCOMM_TEMP_{hash}"));
    if temp_dir.try_exists()? {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    archive.extract(&temp_dir)?;
    let profile_raw = std::fs::read_to_string(temp_dir.join(library::PROFILE_DATA_NAME))?;
    let mut profile: library::Profile = serde_json::from_str(&profile_raw)?;
    profile.name = rename.clone();

    std::fs::write(
        temp_dir.join(library::PROFILE_DATA_NAME),
        serde_json::to_string_pretty(&profile)?,
    )?;
    if destination.try_exists()? {
        std::fs::remove_dir_all(&destination)?;
    }
    files::move_dirs(&temp_dir, destination)?;

    if temp_dir.exists()
        && let Err(err) = std::fs::remove_dir_all(temp_dir)
    {
        eprintln!("Failed to delete temporary directory: {err:?}");
    }

    Ok(profile)
}

pub fn load_incremental_snapshot(
    rename: String,
    destination: &Path,
    mut archive: ZipArchive<File>,
    stop_at: Option<Timestamp>,
) -> std::io::Result<library::Profile> {
    let destination = destination.join(&rename);

    let hash = files::gen_hash(destination.as_os_str().as_encoded_bytes());
    let working_directory = CACHE_DIR.join(format!("LXCOMM_TEMP_{hash}"));
    if working_directory.try_exists()? {
        std::fs::remove_dir_all(&working_directory)?;
    }

    let timestamps = list_incremental_snapshots(&mut archive)?;

    archive.extract(&working_directory)?;

    let root_dir = working_directory.join("root").canonicalize()?;
    let mut cache = HashMap::new();
    for timestamp in timestamps {
        if let Some(stop) = stop_at
            && timestamp > stop
        {
            break;
        }

        let ops_path = working_directory.join(timestamp.to_string());
        let ops = decode_ops(&mut File::open(ops_path)?)?;
        apply_ops(&root_dir, &ops, &mut cache)?;
    }

    let mut profile: library::Profile =
        serde_json::from_slice(&std::fs::read(root_dir.join(library::PROFILE_DATA_NAME))?)?;
    profile.name = rename.clone();
    std::fs::write(
        root_dir.join(library::PROFILE_DATA_NAME),
        serde_json::to_string_pretty(&profile)?,
    )?;

    if destination.try_exists()? {
        std::fs::remove_dir_all(&destination)?;
    }
    files::move_dirs(root_dir, destination)?;
    std::fs::remove_dir_all(working_directory)?;

    Ok(profile)
}

#[test]
fn test_basic_snapshot() -> eyre::Result<()> {
    let backup = PathBuf::from("test.zip");
    if backup.try_exists()? {
        std::fs::remove_file(&backup)?;
    }

    let name = BasicSnapshotBuilder::new(PathBuf::from("test/ExampleProfile"), backup.clone())
        .finalize()?;
    println!("Saved snapshot '{name}'");

    let destination = std::env::temp_dir().join("TestProfiles");
    let rename = "Snapshotted Profile".to_string();
    std::fs::create_dir_all(&destination)?;
    if destination.join(&rename).try_exists()? {
        std::fs::remove_dir_all(destination.join(&rename))?;
    }

    let profile =
        load_basic_snapshot(rename, &destination, ZipArchive::new(File::open(&backup)?)?)?;
    println!("{profile:#?}");
    Ok(())
}

pub trait SnapshotContainer {
    fn snapshot_type(&self) -> SnapshotType;
}

impl SnapshotContainer for ZipArchive<File> {
    fn snapshot_type(&self) -> SnapshotType {
        if let Ok(metadata) = serde_json::from_slice::<SnapshotMetadata>(self.comment()) {
            metadata.snapshot_type
        } else {
            SnapshotType::Invalid
        }
    }
}
