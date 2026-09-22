use std::{
    path::PathBuf,
    sync::{LazyLock, atomic::AtomicU64},
};

use anyhow::{Error, Ok};
use futures_util::StreamExt;
use reqwest::Client;
use tokio::{
    io::AsyncWriteExt,
    sync::{Semaphore, SemaphorePermit},
};

use crate::types::{AssetIndex, AssetsIndex, VersionInfo, VersionManifest};

pub const ESTIMATED_HTTP_OVERHEAD: u64 = 50_000;
pub const MC_VERSION: &str = "26.3";
pub const NF_VERSION: &str = "26.3.0.7-beta";
pub const ASSETS_URL: &str = "https://resources.download.minecraft.net/";
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const INSTANCE: &str = "./instance";

pub static DOWNLOAD_SEM: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(48));

static DOWNLOADED: AtomicU64 = AtomicU64::new(0);
static DOWNLOADING: AtomicU64 = AtomicU64::new(0);

pub fn register_bytes_to_download(bytes: u64) {
    DOWNLOADING.fetch_add(
        bytes + ESTIMATED_HTTP_OVERHEAD,
        std::sync::atomic::Ordering::Relaxed,
    );
}
fn complete_download_bytes(bytes: u64) -> f64 {
    let total = DOWNLOADING.load(std::sync::atomic::Ordering::Relaxed);
    let new = DOWNLOADED.fetch_add(
        bytes + ESTIMATED_HTTP_OVERHEAD,
        std::sync::atomic::Ordering::Relaxed,
    ) + bytes
        + ESTIMATED_HTTP_OVERHEAD;
    new as f64 / total as f64
}

pub async fn download_file(
    client: &Client,
    url: &str,
    path: &str,
    len: u64,
    mkdir: bool,
    sem: Option<SemaphorePermit<'static>>,
) -> Result<(), Error> {
    let sem = match sem {
        Some(sem) => sem,
        None => DOWNLOAD_SEM.acquire().await?,
    };
    let (mut stream, mut file) = tokio::try_join!(
        async { Ok(client.get(url).send().await?.bytes_stream()) },
        async {
            if mkdir {
                let mut path = PathBuf::from(path);
                path.pop();
                tokio::fs::create_dir_all(path).await?;
            }
            let file = tokio::fs::File::create(path).await?;
            file.set_len(len).await?;
            Ok(file)
        }
    )?;
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    drop(sem);
    file.flush().await?;
    let l = complete_download_bytes(len);
    let a = (l * 40.).round() as usize;
    let bar = "#".repeat(a);
    println!("[{bar:-<40}]");
    Ok(())
}
pub async fn load_data<T: serde::de::DeserializeOwned>(
    client: &Client,
    url: &str,
) -> Result<T, Error> {
    Ok(client.get(url).send().await?.json::<T>().await?)
}
pub async fn load_asset_index(client: &Client, asset: &AssetIndex) -> Result<AssetsIndex, Error> {
    let path = format!("{INSTANCE}/assets/indexes/{}.json", asset.id);
    tokio::fs::create_dir_all(format!("{INSTANCE}/assets/indexes/")).await?;
    let (bytes, mut file) = tokio::try_join!(
        async { Ok(client.get(&asset.url).send().await?.bytes().await?) },
        async { Ok(tokio::fs::File::create(&path).await?) }
    )?;
    let (asset_index, ()) = tokio::try_join!(
        async {
            let bytes = bytes.clone();
            Ok(tokio::task::spawn_blocking(move || {
                Ok(serde_json::from_slice::<AssetsIndex>(&bytes)?)
            })
            .await??)
        },
        async { Ok(file.write_all(&bytes).await?) }
    )?;
    Ok(asset_index)
}
pub async fn download_files(client: &Client) -> Result<(), Error> {
    let load_data = async {
        let manifest = load_data::<VersionManifest>(client, VERSION_MANIFEST_URL).await?;
        let version = manifest
            .versions
            .iter()
            .find(|x| x.id == MC_VERSION)
            .unwrap();
        let version_info = load_data::<VersionInfo>(client, &version.url).await?;

        let asset_index = load_asset_index(client, &version_info.asset_index).await?;
        Ok((version_info, asset_index))
    };
    let make_dirs = async {
        let mut set = tokio::task::JoinSet::new();
        set.spawn(async move { Ok(tokio::fs::create_dir(format!("{INSTANCE}/libraries")).await?) });
        for i in 0..=255u8 {
            set.spawn(async move {
                Ok(tokio::fs::create_dir_all(format!("{INSTANCE}/assets/objects/{i:02x}")).await?)
            });
        }
        set.join_all()
            .await
            .into_iter()
            .collect::<Result<(), Error>>()
    };
    let ((version_info, asset_index), ()) = tokio::try_join!(load_data, make_dirs)?;

    let asset_downloads = asset_index.objects.into_values().map(|a| {
        let folder = format!("{}/{}", &a.hash[0..2], a.hash);
        (
            false,
            a.size,
            client.clone(),
            format!("{ASSETS_URL}{folder}"),
            format!("{INSTANCE}/assets/objects/{folder}"),
        )
    });
    let library_downloads = version_info.libraries.iter().map(|i| {
        let download = &i.downloads.artifact;
        (
            true,
            download.size,
            client.clone(),
            download.url.clone(),
            format!("{INSTANCE}/libraries/{}", download.path),
        )
    });
    let mut downloads = asset_downloads.chain(library_downloads).collect::<Vec<_>>();
    let total = downloads
        .iter()
        .map(|&(_, len, _, _, _)| len as u64 + ESTIMATED_HTTP_OVERHEAD)
        .sum::<u64>();
    register_bytes_to_download(total - ESTIMATED_HTTP_OVERHEAD);
    downloads.sort_unstable_by_key(|&(_, size, _, _, _)| std::cmp::Reverse(size));
    let mut set = tokio::task::JoinSet::new();
    for (mkdir, len, client, url, path) in downloads {
        set.spawn(async move {
            Ok(download_file(&client, &url, &path, len as u64, mkdir, None).await?)
        });
    }
    set.join_all()
        .await
        .into_iter()
        .collect::<Result<(), Error>>()?;
    Ok(())
}
