use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Error, Ok};
use either::Either;
use futures_util::StreamExt;
use reqwest::Client;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::types::{AssetIndex, AssetsIndex};

pub const ASSETS_URL: &str = "https://resources.download.minecraft.net/";
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const INSTANCE: &str = "./instance";

pub async fn download_file<P: AsRef<Path>>(
    client: &Client,
    url: &str,
    path: &P,
    size: u64,
) -> Result<(), Error> {
    println!("Downloading: {url}");
    let (mut stream, mut file) = match tokio::try_join!(
        async {
            let response = client.get(url).send().await.map_err(Either::Left)?;
            Result::Ok(response.bytes_stream())
        },
        async {
            let parrent = path.as_ref().parent().unwrap();
            tokio::fs::create_dir_all(parrent)
                .await
                .map_err(Either::Right)?;
            let file = tokio::fs::File::create_new(path)
                .await
                .map_err(Either::Right)?;
            if size != 0 {
                file.set_len(size).await.map_err(Either::Right)?;
            }
            Result::Ok(file)
        }
    ) {
        Result::Ok((stream, file)) => (stream, file),
        Result::Err(Either::Right(e)) if e.kind() == ErrorKind::AlreadyExists => {
            return Ok(());
        }
        Result::Err(e) => return Err(e.either(Error::from, Error::from)),
    };
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    Ok(())
}
pub async fn load_data<T: serde::de::DeserializeOwned>(
    client: &Client,
    url: &str,
) -> Result<T, Error> {
    Ok(client.get(url).send().await?.json::<T>().await?)
}
pub async fn load_asset_index(client: &Client, asset: &AssetIndex) -> Result<AssetsIndex, Error> {
    let mut path = PathBuf::from(INSTANCE);
    path.push("assets/indexes");
    tokio::fs::create_dir_all(&path).await?;
    path.push(format!("{}.json", asset.id));

    if tokio::fs::try_exists(&path).await? {
        let mut bytes = Vec::new();
        tokio::fs::File::open(path)
            .await?
            .read_to_end(&mut bytes)
            .await?;
        let asset_index = serde_json::from_slice::<AssetsIndex>(&bytes)?;
        return Ok(asset_index);
    }

    let (bytes, mut file) = tokio::try_join!(
        async { Ok(client.get(&asset.url).send().await?.bytes().await?) },
        async {
            let file = tokio::fs::File::create(&path).await?;
            file.set_len(asset.size as u64).await?;
            Ok(file)
        }
    )?;
    let (asset_index, ()) = tokio::try_join!(
        async { Ok(serde_json::from_slice::<AssetsIndex>(&bytes)?) },
        async { Ok(file.write_all(&bytes).await?) }
    )?;
    Ok(asset_index)
}
