use std::{collections::HashMap, process::Stdio};

use anyhow::{Error, Ok};
use futures_util::StreamExt;
use reqwest::Client;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{
    download::{INSTANCE, MC_VERSION, NF_VERSION, download_files, register_bytes_to_download},
    types::{FeatureRule, Os, OsRule, PartialVersionInfo, Rule, RuleAction, VersionInfo},
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
    let len = client
        .head(&installer_url)
        .send()
        .await?
        .content_length()
        .unwrap();
    let installer_path = format!("{INSTANCE}/install.jar");
    register_bytes_to_download(len);
    let get_jar = async {
        let (mut stream, mut file) = tokio::try_join!(
            async { Ok(client.get(installer_url).send().await?.bytes_stream()) },
            async {
                let file = tokio::fs::File::create(&installer_path).await?;
                file.set_len(len).await?;
                Ok(file)
            }
        )?;
        while let Some(chunk) = stream.next().await {
            let chunk = &*chunk?;
            file.write_all(chunk).await?;
        }
        file.flush().await?;
        Ok(())
    };
    tokio::try_join!(make_profiles, get_jar)?;
    let output = tokio::process::Command::new("java")
        .args(["-jar", &installer_path, "--installClient", INSTANCE])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .unwrap();
    if !output.status.success() {
        return Err(Error::msg("NeoForge failed to install."));
    }
    let output = String::from_utf8_lossy(&output.stdout);
    println!("{output}");
    tokio::fs::remove_file(installer_path).await?;
    Ok(())
}
async fn get_command(state: &Rule) -> Result<Vec<String>, Error> {
    let parse_child = async {
        let child = tokio::fs::File::open(format!(
            "{INSTANCE}/versions/neoforge-{NF_VERSION}/neoforge-{NF_VERSION}.json"
        ))
        .await?
        .into_std()
        .await;
        Ok(
            tokio::task::spawn_blocking(|| serde_json::from_reader::<_, PartialVersionInfo>(child))
                .await??,
        )
    };
    let parse_parrent = async {
        let parrent = tokio::fs::File::open(format!(
            "{INSTANCE}/versions/{MC_VERSION}/{MC_VERSION}.json"
        ))
        .await?
        .into_std()
        .await;
        Ok(
            tokio::task::spawn_blocking(|| serde_json::from_reader::<_, VersionInfo>(parrent))
                .await??,
        )
    };

    let (parrent, child) = tokio::try_join!(parse_parrent, parse_child)?;
    let mut libs = parrent.libraries.clone();
    libs.extend(child.libraries.iter().cloned());
    let mut cps = Vec::new();

    for i in libs {
        let path = format!("libraries/{}", i.downloads.artifact.path);
        let full_path = format!("{INSTANCE}/{path}");
        assert!(std::fs::exists(&full_path)?);
        cps.push(path);
    }

    let cp = cps.join(":");
    let vars = HashMap::<&str, String>::from([
        ("game_directory", ".".into()),
        ("assets_root", "assets".into()),
        ("assets_index_name", parrent.asset_index.id),
        ("library_directory", "libraries".into()),
        ("natives_directory", "natives".into()),
        ("launcher_name", "MCDE".into()),
        ("launcher_version", "0.1".into()),
        ("classpath", cp),
        ("version_name", MC_VERSION.into()),
        ("auth_player_name", "jan_en_ik".into()),
        ("auth_uuid", "".into()),
        ("auth_access_token", "".into()),
        ("clientid", "".into()),
        ("auth_xuid", "".into()),
        ("version_type", "".into()),
    ]);
    let mut args = parrent.arguments.unwrap();
    for i in child.arguments.unwrap() {
        let p = args.get_mut(&i.0).unwrap();
        p.extend(i.1);
    }
    let args = args
        .into_iter()
        .map(|(t, v)| {
            (
                t,
                v.into_iter()
                    .flat_map(|x| x.into_args(state))
                    .map(|mut x| {
                        for (k, v) in &vars {
                            x = x.replace(&format!("${{{k}}}"), v);
                        }
                        x
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let argv: Vec<String> = args
        .get(&types::ArgumentType::DefaultUserJvm)
        .unwrap()
        .iter()
        .cloned()
        .chain(args.get(&types::ArgumentType::Jvm).unwrap().iter().cloned())
        .chain(std::iter::once(child.main_class))
        .chain(
            args.get(&types::ArgumentType::Game)
                .unwrap()
                .iter()
                .cloned(),
        )
        .collect();
    Ok(argv)
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

    let main = async {
        let client = reqwest::ClientBuilder::new().build().unwrap();
        if !tokio::fs::try_exists(INSTANCE).await? {
            tokio::fs::create_dir_all(INSTANCE).await?;
            match tokio::try_join!(download_files(&client), run_neoforge_install(&client)) {
                Result::Ok(((), ())) => {}
                Result::Err(e) => {
                    // tokio::fs::remove_dir_all(INSTANCE).await.unwrap();
                    panic!("{e}");
                }
            }
        }
        let command = get_command(&state).await?;
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
