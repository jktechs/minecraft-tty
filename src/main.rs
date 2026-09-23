use std::{collections::HashMap, io::ErrorKind, process::Stdio};

use anyhow::{Error, Ok};
use either::Either;
use reqwest::Client;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{
    download::{
        ASSETS_URL, INSTANCE, MC_VERSION, NF_VERSION, VERSION_MANIFEST_URL, download_file,
        load_asset_index, load_data,
    },
    types::{
        FeatureRule, Os, OsRule, PartialVersionInfo, Rule, RuleAction, VersionInfo, VersionManifest,
    },
};

mod download;
mod types;

async fn run_neoforge_install(client: &Client) -> Result<(), Error> {
    let make_profiles = async {
        tokio::fs::File::create(format!("{INSTANCE}/launcher_profiles.json"))
            .await?
            .write_all("{\"profiles\":{}}".as_bytes())
            .await?;
        Ok(())
    };
    let installer_url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{NF_VERSION}/neoforge-{NF_VERSION}-installer.jar"
    );
    let size = client
        .head(&installer_url)
        .send()
        .await?
        .content_length()
        .unwrap();
    let installer_path = format!("{INSTANCE}/install.jar");
    let get_jar = download_file(client, &installer_url, &installer_path, size);
    tokio::try_join!(make_profiles, get_jar)?;
    let output = tokio::process::Command::new("java")
        .args(["-jar", "install.jar", "--installClient", "."])
        .current_dir(INSTANCE)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .unwrap();
    if !output.status.success() {
        let err = &*output.stderr;
        let err = String::from_utf8_lossy(err);
        return Err(Error::msg(format!("NeoForge failed to install:\n{err}")));
    }
    tokio::fs::remove_file(installer_path).await?;
    Ok(())
}
async fn read_combined_version(parrent: Option<VersionInfo>) -> Result<Option<VersionInfo>, Error> {
    match tokio::try_join!(
        async {
            if let Some(parrent) = parrent {
                Result::Ok(parrent)
            } else {
                let parrent = tokio::fs::File::open(format!(
                    "{INSTANCE}/versions/{MC_VERSION}/{MC_VERSION}.json"
                ))
                .await
                .map_err(Either::Left)?;
                serde_json::from_reader::<_, VersionInfo>(parrent.into_std().await)
                    .map_err(Either::Right)
            }
        },
        async {
            let child = tokio::fs::File::open(format!(
                "{INSTANCE}/versions/neoforge-{NF_VERSION}/neoforge-{NF_VERSION}.json"
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
        (
            download.size,
            download.url.clone(),
            format!("{INSTANCE}/libraries/{}", download.path),
        )
    });
    for x in library_downloads {
        exist_checks
            .spawn(async move { tokio::fs::try_exists(&x.2).await.map(|e| e.then_some(x)) });
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
async fn format_arguments(
    mut version_info: VersionInfo,
    classpath: String,
    state: &Rule,
) -> Result<Vec<String>, Error> {
    let asset_index = std::mem::take(&mut version_info.asset_index.id);
    let vars = HashMap::<&str, String>::from([
        ("game_directory", ".".into()),
        ("assets_root", "assets".into()),
        ("assets_index_name", asset_index),
        ("library_directory", "libraries".into()),
        ("natives_directory", "natives".into()),
        ("launcher_name", "MCDE".into()),
        ("launcher_version", "0.1".into()),
        ("classpath", classpath),
        ("version_name", MC_VERSION.into()),
        ("auth_player_name", "jan_en_ik".into()),
        ("auth_uuid", "".into()),
        ("auth_access_token", "".into()),
        ("clientid", "".into()),
        ("auth_xuid", "".into()),
        ("version_type", "".into()),
    ]);
    Ok(version_info.arguments(state, &vars))
}

#[tokio::main]
async fn main() {
    let state = Rule {
        action: RuleAction::Allow,
        features: Some(FeatureRule {
            has_custom_resolution: Some(false),
            has_quick_plays_support: Some(false),
            is_demo_user: Some(true),
            is_quick_play_multiplayer: Some(false),
            is_quick_play_realms: Some(false),
            is_quick_play_singleplayer: Some(false),
        }),
        os: Some(OsRule {
            arch: Some("x86_64".into()),
            name: Some(Os::Linux),
            version: None,
        }),
    };

    async {
        let client = reqwest::ClientBuilder::new().build()?;
        let (classpath, version_info) =
            if let Some(version_info) = read_combined_version(None).await? {
                let classpath = classpath(&client, &version_info, &state).await?;
                (classpath, version_info)
            } else {
                let ((mut classpath, version_info), ()) = tokio::try_join!(
                    async {
                        let manifest =
                            load_data::<VersionManifest>(&client, VERSION_MANIFEST_URL).await?;
                        let version = manifest
                            .versions
                            .iter()
                            .find(|x| x.id == MC_VERSION)
                            .unwrap();
                        let version_info = load_data::<VersionInfo>(&client, &version.url).await?;
                        let mut classpath = classpath(&client, &version_info, &state).await?;
                        classpath.push(':');
                        Ok((classpath, version_info))
                    },
                    async { Ok(run_neoforge_install(&client).await?) }
                )?;

                let version_info = read_combined_version(Some(version_info))
                    .await?
                    .ok_or_else(|| Error::msg("Neoforge failed to install properly"))?;
                classpath.push_str(&child_classpath(&client, &version_info, &state).await?);
                (classpath, version_info)
            };
        let args = format_arguments(version_info, classpath, &state).await?;

        // if !tokio::fs::try_exists(INSTANCE).await? {
        //     tokio::fs::create_dir_all(INSTANCE).await?;
        //     match tokio::try_join!(download_files(&client), run_neoforge_install(&client)) {
        //         Result::Ok(((), ())) => {}
        //         Result::Err(e) => {
        //             // tokio::fs::remove_dir_all(INSTANCE).await.unwrap();
        //             panic!("{e}");
        //         }
        //     }
        // }
        // let command = format_command(&state).await?;
        let output = Command::new("java")
            .args(args)
            .current_dir(INSTANCE)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .await?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            panic!("{err}");
        }
        Ok(())
    }
    .await
    .unwrap();
}
