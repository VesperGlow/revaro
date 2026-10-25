//! List ZIP names through the original storage reader; never extract game bytes.
use crate::{auth::extract::AuthUser, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
};
use revaro_core::{
    ApiError,
    features::GameEntries,
    model::{FileKind, FileStatus},
};
use serde::Serialize;
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

// ZipArchive reads only its directory during construction/name enumeration.
// Bound cumulative metadata reads even when a damaged archive requests more.
struct DirectoryReader {
    reader: Box<dyn crate::file_access::ReadSeek>,
    runtime: tokio::runtime::Handle,
    remaining: usize,
}

#[derive(Debug, Serialize)]
pub struct ArchiveEntry {
    pub path: String,
    pub size: u64,
    pub packed: u64,
    pub offset: u64,
    pub method: u8,
    pub crc: u32,
}
impl Read for DirectoryReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(std::io::Error::other(
                "ZIP directory exceeds the metadata read budget",
            ));
        }
        let length = output.len().min(self.remaining);
        let read = self.runtime.block_on(tokio::io::AsyncReadExt::read(
            &mut self.reader,
            &mut output[..length],
        ))?;
        self.remaining -= read;
        Ok(read)
    }
}
impl Seek for DirectoryReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.runtime
            .block_on(tokio::io::AsyncSeekExt::seek(&mut self.reader, position))
    }
}

fn bounded_directory(reader: &mut DirectoryReader) -> std::io::Result<()> {
    let invalid = || std::io::Error::other("invalid or oversized ZIP directory");
    let size = reader.seek(SeekFrom::End(0))?;
    let tail_size = size.min(65_577) as usize;
    reader.seek(SeekFrom::End(-(tail_size as i64)))?;
    let mut tail = vec![0; tail_size];
    reader.read_exact(&mut tail)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            tail[i..].starts_with(b"PK\x05\x06")
                && i + 22 + u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize == tail.len()
        })
        .ok_or_else(invalid)?;
    let count = u16::from_le_bytes([tail[end + 10], tail[end + 11]]);
    let directory_size = u32::from_le_bytes(tail[end + 12..end + 16].try_into().unwrap());
    let offset = u32::from_le_bytes(tail[end + 16..end + 20].try_into().unwrap());
    let (count, directory_size) =
        if count == u16::MAX || directory_size == u32::MAX || offset == u32::MAX {
            let locator_offset = (size - tail_size as u64 + end as u64)
                .checked_sub(20)
                .ok_or_else(invalid)?;
            reader.seek(SeekFrom::Start(locator_offset))?;
            let mut locator = [0; 20];
            reader.read_exact(&mut locator)?;
            if !locator.starts_with(b"PK\x06\x07") {
                return Err(invalid());
            }
            let zip64_offset = u64::from_le_bytes(locator[8..16].try_into().unwrap());
            reader.seek(SeekFrom::Start(zip64_offset))?;
            let mut record = [0; 56];
            reader.read_exact(&mut record)?;
            if !record.starts_with(b"PK\x06\x06") {
                return Err(invalid());
            }
            (
                u64::from_le_bytes(record[32..40].try_into().unwrap()),
                u64::from_le_bytes(record[40..48].try_into().unwrap()),
            )
        } else {
            (count as u64, directory_size as u64)
        };
    if count > 65_536 || directory_size > 4 << 20 {
        return Err(invalid());
    }
    reader.seek(SeekFrom::Start(0))?;
    Ok(())
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<GameEntries>, ApiError> {
    let file = state
        .db
        .call_api(move |c| {
            crate::file_routes::lookup_file(c, &id)
                .map_err(|_| ApiError::not_found("ready file not found"))
        })
        .await?;
    if file.kind != FileKind::File
        || file.status != FileStatus::Ready
        || !file.name.to_ascii_lowercase().ends_with(".zip")
    {
        return Err(ApiError::bad_request("请选择已上传完成的游戏 ZIP"));
    }
    let permit = state
        .zip_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("正在读取其他 ZIP 目录，请稍后重试"))?;
    let object = state
        .files
        .open(&file.object_key)
        .await
        .map_err(|_| ApiError::new(502, "无法读取游戏 ZIP"))?;
    let runtime = tokio::runtime::Handle::current();
    let entries = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut reader = DirectoryReader {
            reader: object.reader,
            runtime,
            remaining: 8 << 20,
        };
        // Bound ZIP64's declared count before ZipArchive reserves metadata storage.
        bounded_directory(&mut reader)
            .map_err(|_| ApiError::bad_request("无法读取 ZIP 目录，归档可能已损坏或目录过大"))?;
        let zip = zip::ZipArchive::new(reader)
            .map_err(|_| ApiError::bad_request("无法读取 ZIP 目录，归档可能已损坏或目录过大"))?;
        if zip.len() > 65_536 {
            return Err(ApiError::bad_request("ZIP 目录条目过多"));
        }
        let mut names = Vec::with_capacity(zip.len());
        for index in 0..zip.len() {
            let name = zip
                .name_for_index(index)
                .and_then(Result::ok)
                .ok_or_else(|| ApiError::bad_request("无法读取 ZIP 文件名"))?;
            names.push(name.into_owned());
        }
        // Match VLFS.findCommonZipPrefix: strip exactly one shared top directory.
        let prefix = names
            .first()
            .and_then(|name| name.split_once('/'))
            .map(|(top, _)| format!("{top}/"))
            .filter(|prefix| names.iter().all(|name| name.starts_with(prefix)));
        let mut entries: Vec<String> = names
            .into_iter()
            .map(|name| match &prefix {
                Some(prefix) => name[prefix.len()..].to_owned(),
                None => name,
            })
            .filter(|name| {
                crate::galgame_routes::validate_entry_for_engine("kirikiri2", name).is_ok()
                    || crate::galgame_routes::validate_entry_for_engine("siglus", name).is_ok()
            })
            .collect();
        entries.sort_by(|a, b| {
            a.matches('/')
                .count()
                .cmp(&b.matches('/').count())
                .then_with(|| a.cmp(b))
        });
        entries.dedup();
        let engines = entries
            .iter()
            .flat_map(|entry| {
                ["kirikiri2", "siglus"].into_iter().filter(move |engine| {
                    crate::galgame_routes::validate_entry_for_engine(engine, entry).is_ok()
                })
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .collect();
        Ok::<_, ApiError>((entries, engines))
    })
    .await
    .map_err(|_| ApiError::internal("读取 ZIP 目录失败"))??;
    Ok(Json(GameEntries {
        entries: entries.0,
        engines: entries.1,
    }))
}

/// Return the seek metadata needed by the browser Siglus VFS. The archive is
/// never extracted; each resource is fetched later with one authorized Range.
pub async fn archive_index(
    state: &Arc<AppState>,
    file: &revaro_core::model::File,
) -> Result<Vec<ArchiveEntry>, ApiError> {
    let permit = state
        .zip_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("正在读取其他 ZIP 目录，请稍后重试"))?;
    let object = state
        .files
        .open(&file.object_key)
        .await
        .map_err(|_| ApiError::new(502, "无法读取游戏 ZIP"))?;
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut reader = DirectoryReader {
            reader: object.reader,
            runtime,
            remaining: 8 << 20,
        };
        bounded_directory(&mut reader)
            .map_err(|_| ApiError::bad_request("无法读取 ZIP 目录，归档可能已损坏或目录过大"))?;
        let mut zip = zip::ZipArchive::new(reader)
            .map_err(|_| ApiError::bad_request("无法读取 ZIP 目录，归档可能已损坏或目录过大"))?;
        if zip.len() > 65_536 {
            return Err(ApiError::bad_request("ZIP 目录条目过多"));
        }
        let mut raw = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            let entry = zip
                .by_index(i)
                .map_err(|_| ApiError::bad_request("无法读取 ZIP 文件信息"))?;
            if entry.is_dir() || entry.is_symlink() {
                continue;
            }
            let name = entry
                .name()
                .map_err(|_| ApiError::bad_request("无法读取 ZIP 文件名"))?
                .replace('\\', "/");
            if name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(ApiError::bad_request("ZIP 包含非法游戏路径"));
            }
            let method = match entry.compression() {
                zip::CompressionMethod::Stored => 0,
                zip::CompressionMethod::Deflated => 8,
                _ => continue,
            };
            raw.push((
                name,
                ArchiveEntry {
                    path: String::new(),
                    size: entry.size(),
                    packed: entry.compressed_size(),
                    offset: entry
                        .data_start()
                        .ok_or_else(|| ApiError::bad_request("无法读取 ZIP 数据偏移"))?,
                    method,
                    crc: entry.crc32(),
                },
            ));
        }
        let prefix = raw
            .first()
            .and_then(|(name, _)| name.split_once('/'))
            .map(|(top, _)| format!("{top}/"))
            .filter(|prefix| raw.iter().all(|(name, _)| name.starts_with(prefix)));
        let mut out = raw
            .into_iter()
            .map(|(name, mut entry)| {
                entry.path = prefix
                    .as_deref()
                    .and_then(|p| name.strip_prefix(p))
                    .unwrap_or(&name)
                    .to_owned();
                entry
            })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    })
    .await
    .map_err(|_| ApiError::internal("读取 ZIP 目录失败"))?
}
