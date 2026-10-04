use std::{path::Path, pin::Pin, task::Poll};

use anyhow::{Context, Result};
use async_compression::futures::bufread::GzipDecoder;
use futures::{AsyncRead, AsyncSeek, AsyncSeekExt, AsyncWrite, io::BufReader};
use sha2::{Digest, Sha256};

use crate::{HttpClient, github::AssetKind};

#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub struct GithubBinaryMetadata {
    pub metadata_version: u64,
    pub digest: Option<String>,
}

impl GithubBinaryMetadata {
    pub async fn read_from_file(metadata_path: &Path) -> Result<GithubBinaryMetadata> {
        let metadata_content = async_fs::read_to_string(metadata_path)
            .await
            .with_context(|| format!("reading metadata file at {metadata_path:?}"))?;
        serde_json::from_str(&metadata_content)
            .with_context(|| format!("parsing metadata file at {metadata_path:?}"))
    }

    pub async fn write_to_file(&self, metadata_path: &Path) -> Result<()> {
        let metadata_content = serde_json::to_string(self)
            .with_context(|| format!("serializing metadata for {metadata_path:?}"))?;
        async_fs::write(metadata_path, metadata_content.as_bytes())
            .await
            .with_context(|| format!("writing metadata file at {metadata_path:?}"))?;
        Ok(())
    }
}

pub async fn download_server_binary(
    http_client: &dyn HttpClient,
    url: &str,
    digest: Option<&str>,
    destination_path: &Path,
    asset_kind: AssetKind,
) -> Result<(), anyhow::Error> {
    log::info!("downloading github artifact from {url}");
    let mut response = http_client
        .get(url, Default::default(), true)
        .await
        .with_context(|| format!("downloading release from {url}"))?;
    let body = response.body_mut();
    match digest {
        Some(expected_sha_256) => {
            let temp_asset_file = tempfile::NamedTempFile::new()
                .with_context(|| format!("creating a temporary file for {url}"))?;
            let (temp_asset_file, _temp_guard) = temp_asset_file.into_parts();
            let mut writer = HashingWriter {
                writer: async_fs::File::from(temp_asset_file),
                hasher: Sha256::new(),
            };
            futures::io::copy(&mut BufReader::new(body), &mut writer)
                .await
                .with_context(|| {
                    format!("saving archive contents into the temporary file for {url}",)
                })?;
            let asset_sha_256 = format!("{:x}", writer.hasher.finalize());

            anyhow::ensure!(
                asset_sha_256 == expected_sha_256,
                "{url} asset got SHA-256 mismatch. Expected: {expected_sha_256}, Got: {asset_sha_256}",
            );
            writer
                .writer
                .seek(std::io::SeekFrom::Start(0))
                .await
                .with_context(|| format!("seeking temporary file {destination_path:?}",))?;
            stream_file_archive(&mut writer.writer, url, destination_path, asset_kind)
                .await
                .with_context(|| {
                    format!("extracting downloaded asset for {url} into {destination_path:?}",)
                })?;
        }
        None => stream_response_archive(body, url, destination_path, asset_kind)
            .await
            .with_context(|| {
                format!("extracting response for asset {url} into {destination_path:?}",)
            })?,
    }
    Ok(())
}

async fn stream_response_archive(
    response: impl AsyncRead + Unpin,
    url: &str,
    destination_path: &Path,
    asset_kind: AssetKind,
) -> Result<()> {
    match asset_kind {
        AssetKind::TarGz => extract_tar_gz(destination_path, url, response).await?,
        AssetKind::Gz => extract_gz(destination_path, url, response).await?,
        AssetKind::Zip => {
            util::archive::extract_zip(destination_path, response).await?;
        }
    };
    Ok(())
}

async fn stream_file_archive(
    file_archive: impl AsyncRead + AsyncSeek + Unpin,
    url: &str,
    destination_path: &Path,
    asset_kind: AssetKind,
) -> Result<()> {
    match asset_kind {
        AssetKind::TarGz => extract_tar_gz(destination_path, url, file_archive).await?,
        AssetKind::Gz => extract_gz(destination_path, url, file_archive).await?,
        #[cfg(not(windows))]
        AssetKind::Zip => {
            util::archive::extract_seekable_zip(destination_path, file_archive).await?;
        }
        #[cfg(windows)]
        AssetKind::Zip => {
            util::archive::extract_zip(destination_path, file_archive).await?;
        }
    };
    Ok(())
}

async fn extract_tar_gz(
    destination_path: &Path,
    url: &str,
    from: impl AsyncRead + Unpin,
) -> Result<(), anyhow::Error> {
    let decompressed_bytes = GzipDecoder::new(BufReader::new(from));
    let archive = async_tar::Archive::new(decompressed_bytes);
    archive
        .unpack(&destination_path)
        .await
        .with_context(|| format!("extracting {url} to {destination_path:?}"))?;
    Ok(())
}

async fn extract_gz(
    destination_path: &Path,
    url: &str,
    from: impl AsyncRead + Unpin,
) -> Result<(), anyhow::Error> {
    let mut decompressed_bytes = GzipDecoder::new(BufReader::new(from));
    let mut file = async_fs::File::create(&destination_path)
        .await
        .with_context(|| {
            format!("creating a file {destination_path:?} for a download from {url}")
        })?;
    futures::io::copy(&mut decompressed_bytes, &mut file)
        .await
        .with_context(|| format!("extracting {url} to {destination_path:?}"))?;
    Ok(())
}

struct HashingWriter<W: AsyncWrite + Unpin> {
    writer: W,
    hasher: Sha256,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for HashingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, std::io::Error>> {
        match Pin::new(&mut self.writer).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                self.hasher.update(&buf[..n]);
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        Pin::new(&mut self.writer).poll_close(cx)
    }
}

#[cfg(test)]
mod tests {
    use async_compression::futures::write::GzipEncoder;
    use async_tar::{EntryType, Header};
    use futures::{AsyncWriteExt, io::Cursor};

    use super::extract_tar_gz;

    fn append_entry(
        archive: &mut Vec<u8>,
        path: &str,
        entry_type: EntryType,
        mode: u32,
        body: &[u8],
    ) {
        let mut header = Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_entry_type(entry_type);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(body.len() as u64);
        header.set_cksum();
        archive.extend_from_slice(header.as_bytes());
        archive.extend_from_slice(body);
        let padding = (512 - body.len() % 512) % 512;
        archive.extend(std::iter::repeat_n(0, padding));
    }

    async fn gzip_tar(archive: Vec<u8>) -> Cursor<Vec<u8>> {
        let mut encoder = GzipEncoder::new(Cursor::new(Vec::new()));
        encoder.write_all(&archive).await.unwrap();
        encoder.close().await.unwrap();
        let mut cursor = encoder.into_inner();
        cursor.set_position(0);
        cursor
    }

    #[test]
    fn test_extract_tar_gz_preserves_content_and_permissions() {
        futures::executor::block_on(async {
            let content: Vec<u8> = (0u16..1024).map(|b| (b % 256) as u8).collect();
            let mut archive = Vec::new();
            append_entry(
                &mut archive,
                "nested/file.txt",
                EntryType::Regular,
                0o755,
                &content,
            );
            archive.extend_from_slice(&[0u8; 1024]);
            let reader = gzip_tar(archive).await;

            let dir = tempfile::tempdir().unwrap();
            extract_tar_gz(dir.path(), "test://archive.tar.gz", reader)
                .await
                .unwrap();

            let extracted = dir.path().join("nested/file.txt");
            assert_eq!(std::fs::read(&extracted).unwrap(), content);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&extracted).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o755);
            }
        });
    }

    #[test]
    fn test_extract_tar_gz_pax_size_before_gnu_longname() {
        futures::executor::block_on(async {
            let mut archive = Vec::new();
            append_entry(
                &mut archive,
                "PaxHeader",
                EntryType::XHeader,
                0o644,
                b"13 size=1024\n",
            );
            append_entry(
                &mut archive,
                "././@LongLink",
                EntryType::GNULongName,
                0o644,
                b"nested/pax-size.txt\0",
            );
            append_entry(
                &mut archive,
                "placeholder",
                EntryType::Regular,
                0o644,
                &vec![b'A'; 1024],
            );
            // A later entry with its own size. The pax `size` above must not leak
            // onto this header or body.
            append_entry(
                &mut archive,
                "nested/after.txt",
                EntryType::Regular,
                0o644,
                b"ok",
            );
            archive.extend_from_slice(&[0u8; 1024]);
            let reader = gzip_tar(archive).await;

            let dir = tempfile::tempdir().unwrap();
            extract_tar_gz(dir.path(), "test://archive.tar.gz", reader)
                .await
                .unwrap();

            let extracted = dir.path().join("nested/pax-size.txt");
            assert!(extracted.exists(), "nested/pax-size.txt not extracted");
            assert_eq!(std::fs::read(&extracted).unwrap(), vec![b'A'; 1024]);
            assert!(
                !dir.path().join("placeholder").exists(),
                "placeholder file must not be created"
            );
            assert_eq!(
                std::fs::read(dir.path().join("nested/after.txt")).unwrap(),
                b"ok"
            );
        });
    }
}
