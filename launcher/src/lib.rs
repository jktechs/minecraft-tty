use std::{borrow::Cow, collections::HashMap, io::ErrorKind};

use anyhow::{Error, Ok};
use either::Either;
use futures_util::FutureExt;
use reqwest::Client;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{
    download::{
        ASSETS_URL, INSTANCE, VERSION_MANIFEST_URL, download_file, load_asset_index, load_data,
    },
    types::{PartialVersionInfo, Rule, VersionInfo, VersionManifest},
};

pub mod download;
pub mod types;

async fn run_neoforge_install(client: &Client, nf_version: &str) -> Result<(), Error> {
    let make_profiles = async {
        tokio::fs::File::create(format!("{INSTANCE}/launcher_profiles.json"))
            .await?
            .write_all("{\"profiles\":{}}".as_bytes())
            .await?;
        Ok(())
    };
    let installer_url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{nf_version}/neoforge-{nf_version}-installer.jar"
    );
    let size = client
        .head(&installer_url)
        .send()
        .await?
        .content_length()
        .ok_or_else(|| Error::msg("No way to confirm neoforge installer jar size."))?;
    let installer_path = format!("{INSTANCE}/install.jar");
    let get_jar = download_file(client, &installer_url, &installer_path, size);
    tokio::try_join!(make_profiles, get_jar)?;
    let output = tokio::process::Command::new("java")
        .args(["-jar", "install.jar", "--installClient", "."])
        .current_dir(INSTANCE)
        .output()
        .await?;
    if !output.status.success() {
        let err = &*output.stderr;
        let err = String::from_utf8_lossy(err);
        return Err(Error::msg(format!("NeoForge failed to install:\n{err}")));
    }
    tokio::fs::remove_file(installer_path).await?;
    Ok(())
}
async fn read_combined_version(
    parrent: Option<VersionInfo>,
    mc_version: &str,
    nf_version: &str,
) -> Result<Option<VersionInfo>, Error> {
    match tokio::try_join!(
        async {
            if let Some(parrent) = parrent {
                Result::Ok(parrent)
            } else {
                let parrent = tokio::fs::File::open(format!(
                    "{INSTANCE}/versions/{mc_version}/{mc_version}.json"
                ))
                .await
                .map_err(Either::Left)?;
                serde_json::from_reader::<_, VersionInfo>(parrent.into_std().await)
                    .map_err(Either::Right)
            }
        },
        async {
            let child = tokio::fs::File::open(format!(
                "{INSTANCE}/versions/neoforge-{nf_version}/neoforge-{nf_version}.json"
            ))
            .await
            .map_err(Either::Left)?;
            serde_json::from_reader::<_, PartialVersionInfo>(child.into_std().await)
                .map_err(Either::Right)
        }
    ) {
        Result::Ok((parrent, child)) => Ok(Some(parrent.merge(child))),
        Result::Err(Either::Left(e)) if e.kind() == ErrorKind::NotFound => Ok(None),
        Result::Err(e) => Err(e.either(Error::from, Error::from)),
    }
}
async fn child_classpath(
    client: &Client,
    version_info: &VersionInfo,
    state: &Rule,
) -> Result<String, Error> {
    let mut exist_checks = tokio::task::JoinSet::new();
    let library_downloads = version_info.child_libraries(state).map(|i| {
        let download = &i.downloads.artifact;
        assert!(i.downloadable, "!downloadable: {}", download.path);
        assert!(
            i.include_in_classpath,
            "!include_in_classpath: {}",
            download.path
        );
        assert!(
            i.downloads.classifiers.is_none(),
            "classifier: {}",
            download.path
        );
        (
            download.size,
            download.url.clone(),
            format!("{INSTANCE}/libraries/{}", download.path),
        )
    });
    for x in library_downloads {
        exist_checks
            .spawn(async move { tokio::fs::try_exists(&x.2).await.map(|e| (!e).then_some(x)) });
    }
    let mut downloads = Vec::new();
    while let Some(x) = exist_checks.join_next().await {
        if let Some(x) = x?? {
            downloads.push(x);
        }
    }
    downloads.sort_unstable_by_key(|&(size, _, _)| std::cmp::Reverse(size));
    let mut download_set = tokio::task::JoinSet::new();
    for (size, url, path) in downloads {
        let client = client.clone();
        download_set.spawn(async move { download_file(&client, &url, &path, size as u64).await });
    }
    while let Some(x) = download_set.join_next().await {
        x??;
    }

    let cp = version_info
        .child_libraries(state)
        .map(|x| format!("libraries/{}", x.downloads.artifact.path))
        .collect::<Vec<_>>()
        .join(":");
    Ok(cp)
}
async fn classpath(
    client: &Client,
    version_info: &VersionInfo,
    state: &Rule,
) -> Result<String, Error> {
    let asset_index = load_asset_index(client, &version_info.asset_index).await?;

    let mut exist_checks = tokio::task::JoinSet::new();
    let asset_downloads = asset_index.objects.into_values().map(|a| {
        let folder = format!("{}/{}", &a.hash[0..2], a.hash);
        (
            a.size,
            format!("{ASSETS_URL}{folder}"),
            format!("{INSTANCE}/assets/objects/{folder}"),
        )
    });
    let library_downloads = version_info.libraries(state).map(|i| {
        let download = &i.downloads.artifact;
        assert!(i.downloadable, "!downloadable: {}", download.path);
        assert!(
            i.include_in_classpath,
            "!include_in_classpath: {}",
            download.path
        );
        assert!(
            i.downloads.classifiers.is_none(),
            "classifier: {}",
            download.path
        );
        (
            download.size,
            download.url.clone(),
            format!("{INSTANCE}/libraries/{}", download.path),
        )
    });
    for x in asset_downloads.chain(library_downloads) {
        exist_checks
            .spawn(async move { tokio::fs::try_exists(&x.2).await.map(|e| (!e).then_some(x)) });
    }
    let mut downloads = Vec::new();
    while let Some(x) = exist_checks.join_next().await {
        if let Some(x) = x?? {
            downloads.push(x);
        }
    }
    downloads.sort_unstable_by_key(|&(size, _, _)| std::cmp::Reverse(size));
    let mut download_set = tokio::task::JoinSet::new();
    for (size, url, path) in downloads {
        let client = client.clone();
        download_set.spawn(async move { download_file(&client, &url, &path, size as u64).await });
    }
    while let Some(x) = download_set.join_next().await {
        x??;
    }

    let cp = version_info
        .libraries(state)
        .map(|x| format!("libraries/{}", x.downloads.artifact.path))
        .collect::<Vec<_>>()
        .join(":");

    Ok(cp)
}
async fn format_arguments<'a>(
    mut version_info: VersionInfo,
    classpath: &'a str,
    state: &Rule,
    mc_version: &'a str,
    extra_vars: &'a HashMap<&'static str, Cow<'a, str>>,
) -> Result<Vec<String>, Error> {
    let asset_index = std::mem::take(&mut version_info.asset_index.id);
    let mut vars = HashMap::<&'static str, Cow<str>>::from([
        ("game_directory", ".".into()),
        ("assets_root", "assets".into()),
        ("assets_index_name", asset_index.into()),
        ("library_directory", "libraries".into()),
        ("natives_directory", "natives".into()),
        ("launcher_name", "MCDE".into()),
        ("launcher_version", "0.1".into()),
        ("classpath", classpath.into()),
        ("version_name", mc_version.into()),
        ("auth_player_name", "jan_en_ik".into()),
        ("auth_uuid", "".into()),
        ("auth_access_token", "".into()),
        ("clientid", "".into()),
        ("auth_xuid", "".into()),
        ("version_type", "".into()),
    ]);
    vars.extend(extra_vars.iter().map(|(k, v)| (*k, v.clone())));
    let args = version_info.arguments(state, &vars);
    Ok(args)
}

pub async fn run_async<'a>(
    state: &Rule,
    mc_version: &'a str,
    nf_version: &'a str,
    extra_vars: &'a HashMap<&'static str, Cow<'a, str>>,
) -> Result<tokio::process::Command, Error> {
    let client = reqwest::ClientBuilder::new().build()?;
    let (classpath, version_info) = if let Some(version_info) =
        read_combined_version(None, mc_version, nf_version).await?
    {
        let classpath = classpath(&client, &version_info, state).await?;
        (classpath, version_info)
    } else {
        let ((mut classpath, version_info), ()) = tokio::try_join!(
            async {
                let manifest = load_data::<VersionManifest>(&client, VERSION_MANIFEST_URL).await?;
                let version = manifest
                    .versions
                    .iter()
                    .find(|x| x.id == mc_version)
                    .unwrap();
                let version_info = load_data::<VersionInfo>(&client, &version.url).await?;
                let mut classpath = classpath(&client, &version_info, state).await?;
                classpath.push(':');
                Ok((classpath, version_info))
            },
            async { Ok(run_neoforge_install(&client, nf_version).await?) }
        )?;

        let version_info = read_combined_version(Some(version_info), mc_version, nf_version)
            .await?
            .ok_or_else(|| Error::msg("Neoforge failed to install properly"))?;
        classpath.push_str(&child_classpath(&client, &version_info, state).await?);
        (classpath, version_info)
    };
    let args = format_arguments(version_info, &classpath, state, mc_version, extra_vars).await?;

    let mut command = Command::new("java");
    command.args(args).current_dir(INSTANCE);
    Ok(command)
}
pub fn run_sync(
    state: Rule,
    mc_version: String,
    nf_version: String,
    extra_vars: HashMap<&'static str, Cow<'static, str>>,
) -> Result<std::process::Command, Error> {
    match std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed building the Runtime")
            .block_on(
                run_async(&state, &mc_version, &nf_version, &extra_vars)
                    .map(|e| e.map(|c| c.into_std())),
            )
    })
    .join()
    {
        Result::Ok(c) => c,
        Result::Err(e) => std::panic::resume_unwind(e),
    }
}
