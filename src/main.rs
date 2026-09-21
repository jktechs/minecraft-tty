use std::{io::Read, process::Stdio};

use anyhow::{Error, Ok};
use futures_util::StreamExt;
use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tokio_util::bytes::{Buf, Bytes};

use crate::types::{AssetIndex, AssetsIndex, VersionInfo, VersionManifest};

mod types;

const MC_VERSION: &str = "26.3";
const NF_VERSION: &str = "26.3.0.7-beta";
const ASSETS: &str = "https://resources.download.minecraft.net/";
/// The URL to the version manifest
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
async fn asset_url(hash: &str) -> Result<String, Error> {
    let folder = &hash[0..2];
    if !tokio::fs::try_exists(format!("./data/assets/objects/{}", folder)).await? {
        tokio::fs::create_dir_all(format!("./data/assets/objects/{}", folder)).await?;
    }
    Ok(format!("{folder}/{hash}"))
}
fn neoforge_installer() -> String {
    format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{NF_VERSION}/neoforge-{NF_VERSION}-installer.jar"
    )
}
async fn download_file(client: &Client, url: &str, path: &str) -> Result<(), Error> {
    println!("Downloading: {url} to {path}");
    let request = async {
        let request = client.get(url);
        let response = request.send().await?;
        Ok(response.bytes_stream())
    };
    let file = async { Ok(tokio::fs::File::create(path).await?) };
    let (stream, mut file) = tokio::try_join!(request, file)?;
    let mut stream = Box::pin(stream);
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    Ok(())
}
async fn load_data<T: serde::de::DeserializeOwned>(client: &Client, url: &str) -> Result<T, Error> {
    println!("Loading: {url}");
    let request = client.get(url);
    let response = request.send().await?;
    Ok(response.json::<T>().await?)
}
async fn load_asset_index(client: &Client, asset: &AssetIndex) -> Result<AssetsIndex, Error> {
    let path = format!("./data/assets/indexes/{}.json", asset.id);
    println!("Downloading/Loading: {path}");
    tokio::fs::create_dir_all("./data/assets/indexes/").await?;
    let request = async {
        let request = client.get(&asset.url);
        let response = request.send().await?;
        Ok(response.bytes_stream())
    };
    let file = async { Ok(tokio::fs::File::create(path).await?) };
    let (stream, mut file) = tokio::try_join!(request, file)?;
    let mut stream = Box::pin(stream);
    let (tx, rx) = std::sync::mpsc::channel::<Bytes>();

    let async_driver = async move {
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            tx.send(bytes.clone())?;
            file.write_all(&bytes).await?;
        }
        file.flush().await?;
        Ok(())
    };
    let sync_driver = async {
        Ok(tokio::task::spawn_blocking(move || {
            let rx = rx;
            struct ChannelReader(std::sync::mpsc::Receiver<Bytes>, Bytes);
            impl std::io::Read for ChannelReader {
                fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                    if self.1.is_empty() {
                        match self.0.recv() {
                            Result::Ok(bytes) => {
                                self.1 = bytes;
                                self.read(buf)
                            }
                            Result::Err(std::sync::mpsc::RecvError) => Result::Ok(0),
                        }
                    } else {
                        let len = self.1.len().min(buf.len());
                        buf[..len].copy_from_slice(&self.1[..len]);
                        self.1.advance(len);
                        Result::Ok(len)
                    }
                }
            }
            let asset_index =
                serde_json::from_reader::<_, AssetsIndex>(ChannelReader(rx, Bytes::new()))?;
            Ok(asset_index)
        })
        .await??)
    };
    let (index, ()) = tokio::try_join!(sync_driver, async_driver)?;
    Ok(index)
}
async fn run_neoforge_install(client: &Client) -> Result<(), Error> {
    let make_profiles = async {
        tokio::fs::File::create("./data/launcher_profiles.json")
            .await?
            .write_all("{\"profiles\":{}}".as_bytes())
            .await?;
        Ok(())
    };
    let installer_path = neoforge_installer();
    let get_jar = download_file(client, &installer_path, "./data/install.jar");
    tokio::try_join!(make_profiles, get_jar)?;
    println!("Running: NeoForge");
    let output = tokio::process::Command::new("java")
        .args(["-jar", "./data/install.jar", "--installClient", "./data"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .await
        .unwrap();
    if !output.status.success() {
        return Err(Error::msg("NeoForge failed to initialize."));
    }
    tokio::fs::remove_file("./data/install.jar").await?;
    Ok(())
}
async fn get_command() -> Result<(), Error> {
    let parse_child = async {
        Ok(tokio::task::spawn_blocking(|| {
            let mut child = std::fs::File::open(format!(
                "./data/versions/neoforge-{NF_VERSION}/neoforge-{NF_VERSION}.json"
            ))?;
            let mut file = String::new();
            child.read_to_string(&mut file)?;
            Ok(file)
        })
        .await??)
    };
    let parse_parrent = async {
        Ok(tokio::task::spawn_blocking(|| {
            let parrent =
                std::fs::File::open(format!("./data/versions/{MC_VERSION}/{MC_VERSION}.json"))?;
            let info = serde_json::from_reader::<_, VersionInfo>(parrent)?;
            Ok(info)
        })
        .await??)
    };
    let (parrent, child) = tokio::try_join!(parse_parrent, parse_child)?;
    println!("{parrent:?} {child}");
    Ok(())
}
async fn download_assets(client: &Client) -> Result<(), Error> {
    let make_dir = async { Ok(tokio::fs::create_dir_all("./data/assets/objects").await?) };

    let get_assets = async {
        let manifest = load_data::<VersionManifest>(client, VERSION_MANIFEST_URL).await?;
        let version = manifest
            .versions
            .iter()
            .find(|x| x.id == MC_VERSION)
            .unwrap();
        let version_info = load_data::<VersionInfo>(client, &version.url).await?;

        let asset_index = load_asset_index(client, &version_info.asset_index).await?;

        let mut assets = asset_index.objects.into_values().collect::<Vec<_>>();
        let assets = tokio::task::spawn_blocking(move || {
            assets.sort_unstable_by_key(|a| std::cmp::Reverse(a.size));
            assets
        })
        .await?;
        let mut file_downloads = assets.into_iter().map(|a| {
            let client = client.clone();
            async move {
                let url = asset_url(&a.hash).await?;
                Ok(download_file(
                    &client,
                    &format!("{ASSETS}{url}"),
                    &format!("./data/assets/objects/{url}"),
                )
                .await)
            }
        });
        let mut set = tokio::task::JoinSet::new();
        for i in (&mut file_downloads).take(10) {
            set.spawn(i);
        }
        while let Some(result) = set.join_next().await {
            result???;
            if let Some(next) = file_downloads.next() {
                set.spawn(next);
            }
        }
        Ok(())
    };
    tokio::try_join!(make_dir, get_assets)?;
    Ok(())
}

#[tokio::main]
async fn main() {
    let client = reqwest::ClientBuilder::new().build().unwrap();

    let main = async {
        if !tokio::fs::try_exists("./data").await? {
            match tokio::try_join!(download_assets(&client), run_neoforge_install(&client)) {
                Result::Ok(((), ())) => {}
                Result::Err(e) => {
                    // tokio::fs::remove_dir_all("./data").await.unwrap();
                    panic!("{e}");
                }
            }
        }
        let command = get_command().await?;
        Ok(())
    };
    main.await.unwrap()
}
