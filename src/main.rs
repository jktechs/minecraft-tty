use std::{collections::HashMap, path::PathBuf, process::Stdio};

use anyhow::{Error, Ok};
use futures_util::StreamExt;
use reqwest::Client;
use tokio::{io::AsyncWriteExt, process::Command};
use tokio_util::bytes::{Buf, Bytes};

use crate::types::{
    Argument, ArgumentValue, AssetIndex, AssetsIndex, Os, PartialVersionInfo, RuleAction,
    VersionInfo, VersionManifest,
};

mod types;

const INSTANCE: &str = "./instance";

const MC_VERSION: &str = "26.3";
const NF_VERSION: &str = "26.3.0.7-beta";
const ASSETS: &str = "https://resources.download.minecraft.net/";
/// The URL to the version manifest
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
async fn asset_url(hash: &str) -> Result<String, Error> {
    let folder = &hash[0..2];
    let path = format!("{INSTANCE}/assets/objects/{}", folder);
    if !tokio::fs::try_exists(&path).await? {
        tokio::fs::create_dir_all(path).await?;
    }
    Ok(format!("{folder}/{hash}"))
}
fn neoforge_installer() -> String {
    format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{NF_VERSION}/neoforge-{NF_VERSION}-installer.jar"
    )
}
async fn download_file(client: &Client, url: &str, path: &str) -> Result<(), Error> {
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
    let request = client.get(url);
    let response = request.send().await?;
    Ok(response.json::<T>().await?)
}
async fn load_asset_index(client: &Client, asset: &AssetIndex) -> Result<AssetsIndex, Error> {
    let path = format!("{INSTANCE}/assets/indexes/{}.json", asset.id);
    println!("Downloading/Loading: {path}");
    tokio::fs::create_dir_all(format!("{INSTANCE}/assets/indexes/")).await?;
    let request = async {
        let request = client.get(&asset.url);
        let response = request.send().await?;
        Ok(response.bytes_stream())
    };
    let file = async { Ok(tokio::fs::File::create(&path).await?) };
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
    println!("Loaded: {path}");
    Ok(index)
}
async fn run_neoforge_install(client: &Client) -> Result<(), Error> {
    println!("Downloading: NeoForge");
    let make_profiles = async {
        tokio::fs::File::create(format!("{INSTANCE}/launcher_profiles.json"))
            .await?
            .write_all("{\"profiles\":{}}".as_bytes())
            .await?;
        Ok(())
    };
    let installer_url = neoforge_installer();
    let installer_path = format!("{INSTANCE}/install.jar");
    let get_jar = download_file(client, &installer_url, &installer_path);
    tokio::try_join!(make_profiles, get_jar)?;
    println!("Running: NeoForge");
    let output = tokio::process::Command::new("java")
        .args(["-jar", &installer_path, "--installClient", INSTANCE])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .await
        .unwrap();
    if !output.status.success() {
        return Err(Error::msg("NeoForge failed to initialize."));
    }
    tokio::fs::remove_file(installer_path).await?;
    println!("Done: NeoForge");
    Ok(())
}
async fn get_command() -> Result<Vec<String>, Error> {
    println!("Loading Command");
    let parse_child = async {
        Ok(tokio::task::spawn_blocking(|| {
            let child = std::fs::File::open(format!(
                "{INSTANCE}/versions/neoforge-{NF_VERSION}/neoforge-{NF_VERSION}.json"
            ))?;
            let value = serde_json::from_reader::<_, PartialVersionInfo>(child)?;
            Ok(value)
        })
        .await??)
    };
    let parse_parrent = async {
        Ok(tokio::task::spawn_blocking(|| {
            let parrent = std::fs::File::open(format!(
                "{INSTANCE}/versions/{MC_VERSION}/{MC_VERSION}.json"
            ))?;
            let info = serde_json::from_reader::<_, VersionInfo>(parrent)?;
            Ok(info)
        })
        .await??)
    };

    let (parrent, child) = tokio::try_join!(parse_parrent, parse_child)?;

    println!("Parsing args");
    let mut args = parrent.arguments.unwrap();
    for i in child.arguments.unwrap() {
        let p = args.get_mut(&i.0).unwrap();
        p.extend(i.1);
    }
    let mut args = args
        .into_iter()
        .map(|(t, v)| {
            (
                t,
                v.into_iter()
                    .flat_map(|x| match x {
                        Argument::Normal(n) => vec![n],
                        Argument::Ruled { rules: None, value } => match value {
                            ArgumentValue::Single(n) => vec![n],
                            ArgumentValue::Many(n) => n,
                        },
                        Argument::Ruled { rules, value } => {
                            let rules = rules.unwrap_or(Vec::new());
                            for i in rules {
                                assert!(matches!(i.action, RuleAction::Allow));
                                if let Some(os) = i.os {
                                    if os.arch.is_some_and(|x| x != "x86_64") {
                                        return vec![];
                                    }
                                    if os.name.is_some_and(|x| x != Os::Linux) {
                                        return vec![];
                                    }
                                    if os.version.is_some() {
                                        panic!("fuck if i know")
                                    }
                                }
                                if let Some(feature) = i.features {
                                    if feature.has_custom_resolution == Some(true)
                                        || feature.is_quick_play_multiplayer == Some(true)
                                        || feature.is_quick_play_realms == Some(true)
                                        || feature.has_quick_plays_support == Some(true)
                                        || feature.is_demo_user == Some(false)
                                        || feature.is_quick_play_singleplayer == Some(true)
                                    {
                                        return vec![];
                                    }
                                }
                            }
                            match value {
                                ArgumentValue::Single(n) => vec![n],
                                ArgumentValue::Many(n) => n,
                            }
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut libs = parrent.libraries.clone();
    libs.extend(child.libraries.iter().cloned());
    let mut cps = Vec::new();
    let mut set = tokio::task::JoinSet::new();

    println!("Downloading libs");
    for i in libs {
        let path = format!(
            "libraries/{}",
            i.downloads
                .as_ref()
                .unwrap()
                .artifact
                .as_ref()
                .unwrap()
                .path
                .as_ref()
                .unwrap()
        );
        if !std::fs::exists(&path)? {
            let url = i
                .downloads
                .as_ref()
                .unwrap()
                .artifact
                .as_ref()
                .unwrap()
                .url
                .clone();
            let path = format!("{INSTANCE}/{path}");
            set.spawn(async move {
                let mut dir_path = PathBuf::from(&path);
                dir_path.pop();
                tokio::fs::create_dir_all(&dir_path).await?;
                let bytes = reqwest::get(url).await?.bytes().await?;
                tokio::fs::File::create(&path)
                    .await?
                    .write_all(&bytes)
                    .await?;
                Ok(())
            });
        }
        cps.push(path);
    }
    set.join_all()
        .await
        .into_iter()
        .collect::<Result<(), _>>()?;
    println!("Downloaded libs");
    let cp = cps.join(":");
    let vars = HashMap::<&str, String>::from([
        ("game_directory", ".".into()),
        ("assets_root", "assets".into()),
        ("assets_index_name", "34".into()),
        ("library_directory", "libraries".into()),
        ("natives_directory", "natives".into()),
        ("launcher_name", "MCDE".into()),
        ("launcher_version", "0.1".into()),
        ("classpath", cp),
        ("version_name", "26.3".into()),
        ("auth_player_name", "jan_en_ik".into()),
        ("auth_uuid", "".into()),
        ("auth_access_token", "".into()),
        ("clientid", "".into()),
        ("auth_xuid", "".into()),
        ("version_type", "".into()),
    ]);
    args.values_mut().flat_map(|v| v.iter_mut()).for_each(|x| {
        for (k, v) in &vars {
            *x = x.replace(&format!("${{{k}}}"), v);
        }
    });
    let argv: Vec<String> = args
        .get(&types::ArgumentType::DefaultUserJvm)
        .unwrap()
        .iter()
        .cloned()
        .chain(args.get(&types::ArgumentType::Jvm).unwrap().iter().cloned())
        .chain(std::iter::once(
            "net.neoforged.fml.startup.Client".to_string(),
        ))
        .chain(
            args.get(&types::ArgumentType::Game)
                .unwrap()
                .iter()
                .cloned(),
        )
        .collect();
    println!("Replaced vars");
    Ok(argv)
}
async fn download_assets(client: &Client) -> Result<(), Error> {
    println!("Downloading Assets");
    let make_dir =
        async { Ok(tokio::fs::create_dir_all(format!("{INSTANCE}/assets/objects")).await?) };

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
        let file_downloads = assets.into_iter().map(|a| {
            let client = client.clone();
            async move {
                let url = asset_url(&a.hash).await?;
                Ok(download_file(
                    &client,
                    &format!("{ASSETS}{url}"),
                    &format!("{INSTANCE}/assets/objects/{url}"),
                )
                .await)
            }
        });
        let mut set = tokio::task::JoinSet::new();
        for i in file_downloads {
            set.spawn(i);
        }
        while let Some(result) = set.join_next().await {
            result???;
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
        if !tokio::fs::try_exists(INSTANCE).await? {
            match tokio::try_join!(download_assets(&client), run_neoforge_install(&client)) {
                Result::Ok(((), ())) => {}
                Result::Err(e) => {
                    // tokio::fs::remove_dir_all(INSTANCE).await.unwrap();
                    panic!("{e}");
                }
            }
        }
        let command = get_command().await?;
        println!("Running");
        Command::new("java")
            .args(command)
            .current_dir(INSTANCE)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .await?;
        Ok(())
    };
    main.await.unwrap()
}
