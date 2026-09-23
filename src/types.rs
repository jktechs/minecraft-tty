use std::{assert_matches, collections::HashMap};

use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
/// Information about the assets of the game
pub struct AssetIndex {
    /// The game version ID the assets are for
    pub id: String,
    /// The SHA1 hash of the assets index
    pub sha1: String,
    /// The size of the assets index
    pub size: u32,
    /// The size of the game version's assets
    pub total_size: u32,
    /// A URL to a file which contains information about the version's assets
    pub url: String,
}
#[derive(Deserialize, Debug)]
/// An asset of the game
pub struct Asset {
    /// The SHA1 hash of the asset file
    pub hash: String,
    /// The size of the asset file
    pub size: u32,
}

#[derive(Deserialize, Debug)]
/// An index containing all assets the game needs
pub struct AssetsIndex {
    /// A hashmap containing the filename (key) and asset (value)
    pub objects: HashMap<String, Asset>,
}
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
/// Information about a version
pub struct VersionInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Arguments passed to the game or JVM
    pub arguments: HashMap<ArgumentType, Vec<Argument>>,
    /// Assets for the game
    pub asset_index: AssetIndex,
    /// The version ID of the assets
    pub assets: String,
    /// The version ID of the version
    pub id: String,
    /// Libraries that the version depends on
    pub libraries: Vec<Library>,
    /// The classpath to the main class to launch the game
    pub main_class: String,
    #[serde(rename = "type")]
    /// The type of version
    pub type_: VersionType,
    #[serde(skip)]
    pub child_libraries: Vec<Library>,
}
impl VersionInfo {
    pub fn libraries<'a>(&'a self, state: &'a Rule) -> impl Iterator<Item = &'a Library> + 'a {
        state.filter(self.libraries.iter().chain(&self.child_libraries), |l| {
            l.rules.as_deref().unwrap_or(&[])
        })
    }
    pub fn child_libraries<'a>(
        &'a self,
        state: &'a Rule,
    ) -> impl Iterator<Item = &'a Library> + 'a {
        state.filter(&self.child_libraries, |l| l.rules.as_deref().unwrap_or(&[]))
    }
    pub fn merge(mut self, child: PartialVersionInfo) -> Self {
        self.main_class = child.main_class;
        self.type_ = child.type_;
        self.child_libraries.extend(child.libraries);
        for (t, v) in child.arguments {
            self.arguments.entry(t).or_default().extend(v);
        }
        self.id = child.id;
        self
    }
    pub fn arguments(mut self, state: &Rule, vars: &HashMap<&'static str, String>) -> Vec<String> {
        let jvm = self.arguments.remove(&ArgumentType::Jvm).unwrap();
        let game = self.arguments.remove(&ArgumentType::Game).unwrap();
        let user_jvm = self
            .arguments
            .remove(&ArgumentType::DefaultUserJvm)
            .unwrap();
        let jvm = Self::parse_arguments(jvm, state, vars);
        let game = Self::parse_arguments(game, state, vars);
        let user_jvm = Self::parse_arguments(user_jvm, state, vars);
        user_jvm
            .chain(jvm)
            .chain(std::iter::once(self.main_class))
            .chain(game)
            .collect()
    }
    fn parse_arguments(
        args: Vec<Argument>,
        state: &Rule,
        vars: &HashMap<&'static str, String>,
    ) -> impl Iterator<Item = String> {
        state
            .filter(args, Argument::rules)
            .flat_map(Argument::into_arguments)
            .map(|s| {
                vars.iter()
                    .fold(s, |s, (k, v)| s.replace(&format!("${{{k}}}"), v))
            })
    }
}
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
/// Information about a version
pub struct PartialVersionInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Arguments passed to the game or JVM
    pub arguments: HashMap<ArgumentType, Vec<Argument>>,
    /// The version ID of the version
    pub id: String,
    /// Libraries that the version depends on
    pub libraries: Vec<Library>,
    /// The classpath to the main class to launch the game
    pub main_class: String,
    #[serde(rename = "type")]
    /// The type of version
    pub type_: VersionType,
}
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
/// The version type
pub enum VersionType {
    /// A major version, which is stable for all players to use
    Release,
    /// An experimental version, which is unstable and used for feature previews and beta testing
    Snapshot,
    /// The oldest versions before the game was released
    OldAlpha,
    /// Early versions of the game
    OldBeta,
}
#[derive(Deserialize, Debug, Clone)]
/// A list of files that should be downloaded for libraries
pub struct LibraryDownloads {
    // #[serde(skip_serializing_if = "Option::is_none")]
    /// The primary library artifact
    pub artifact: LibraryDownload,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Conditional files that may be needed to be downloaded alongside the library
    /// The HashMap key specifies a classifier as additional information for downloading files
    pub classifiers: Option<HashMap<String, LibraryDownload>>,
}
#[derive(Deserialize, Debug, Clone)]
/// Download information of a library
pub struct LibraryDownload {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The path that the library should be saved to
    pub path: String,
    /// The SHA1 hash of the library
    pub sha1: String,
    /// The size of the library
    pub size: u32,
    /// The URL where the library can be downloaded
    pub url: String,
}
fn default_include_in_classpath() -> bool {
    true
}
fn default_downloadable() -> bool {
    true
}
#[derive(Deserialize, Debug, Clone)]
/// A library which the game relies on to run
pub struct Library {
    // #[serde(skip_serializing_if = "Option::is_none")]
    /// The files the library has
    pub downloads: LibraryDownloads,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Rules deciding whether the library should be downloaded or not
    pub rules: Option<Vec<Rule>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// SHA1 Checksums for validating the library's integrity. Only present for forge libraries
    pub checksums: Option<Vec<String>>,
    #[serde(default = "default_include_in_classpath")]
    /// Whether the library should be included in the classpath at the game's launch
    pub include_in_classpath: bool,
    #[serde(default = "default_downloadable")]
    /// Whether the library should be downloaded
    pub downloadable: bool,
}
#[derive(Deserialize, Debug, Eq, PartialEq, Hash, Clone, Copy)]
#[serde(rename_all = "snake_case")]
/// The type of argument
pub enum ArgumentType {
    /// The argument is passed to the game
    Game,
    /// The argument is passed to the JVM
    Jvm,
    /// Unused defaults
    #[serde(rename = "default-user-jvm")]
    DefaultUserJvm,
}
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
/// A command line argument passed to a program
pub enum Argument {
    /// An argument which is applied no matter what
    Normal(String),
    /// An argument which is only applied if certain conditions are met
    Ruled {
        /// The rules deciding whether the argument(s) is used or not
        rules: Option<Vec<Rule>>,
        /// The container of the argument(s) that should be applied accordingly
        value: ArgumentValue,
    },
}
impl Argument {
    pub fn rules(&self) -> &[Rule] {
        if let Argument::Ruled {
            rules: Some(rules), ..
        } = self
        {
            rules
        } else {
            &[]
        }
    }
    pub fn into_arguments(self) -> Vec<String> {
        match self {
            Self::Normal(s)
            | Self::Ruled {
                value: ArgumentValue::Single(s),
                ..
            } => vec![s],
            Self::Ruled {
                value: ArgumentValue::Many(s),
                ..
            } => s,
        }
    }
}
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
/// A container for an argument or multiple arguments
pub enum ArgumentValue {
    /// The container has one argument
    Single(String),
    /// The container has multiple arguments
    Many(Vec<String>),
}
#[derive(Deserialize, Debug, Clone)]
/// A rule deciding whether a file is downloaded, an argument is used, etc.
pub struct Rule {
    /// The action the rule takes
    pub action: RuleAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The OS rule
    pub os: Option<OsRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The feature rule
    pub features: Option<FeatureRule>,
}
impl Rule {
    pub fn valid_with_state(&self, state: &Self) -> bool {
        assert_matches!(self.action, RuleAction::Allow);
        self.os
            .as_ref()
            .zip(state.os.as_ref())
            .is_none_or(|(a, b)| a.valid_with_state(b))
            && self
                .features
                .as_ref()
                .zip(state.features.as_ref())
                .is_none_or(|(a, b)| a.valid_with_state(b))
    }
    pub fn filter<'a, T, I: IntoIterator<Item = T>, F: (FnMut(&T) -> &[Rule]) + 'a>(
        &'a self,
        iter: I,
        mut key: F,
    ) -> impl Iterator<Item = T> + 'a
    where
        I::IntoIter: 'a,
    {
        iter.into_iter().filter(move |x| {
            let key = key(x);
            key.is_empty() || key.iter().any(|x| x.valid_with_state(self))
        })
    }
}

#[derive(Deserialize, Debug, Clone)]
/// A rule which depends on what OS the user is on
pub struct OsRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The name of the OS
    pub name: Option<Os>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The version of the OS. This is normally a RegEx
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The architecture of the OS
    pub arch: Option<String>,
}
impl OsRule {
    fn valid_with_state(&self, state: &Self) -> bool {
        self.name
            .as_ref()
            .zip(state.name.as_ref())
            .is_none_or(|(a, b)| a == b)
            && self
                .version
                .as_ref()
                .zip(state.version.as_ref())
                .is_none_or(|(a, b)| a == b)
            && self
                .arch
                .as_ref()
                .zip(state.arch.as_ref())
                .is_none_or(|(a, b)| a == b)
    }
}
#[derive(Deserialize, Debug, Clone)]
/// A rule which depends on the toggled features of the launcher
pub struct FeatureRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Whether the user is in demo mode
    pub is_demo_user: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Whether the user is using a custom resolution
    pub has_custom_resolution: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Whether the launcher has quick plays support
    pub has_quick_plays_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Whether the instance is being launched to a single-player world
    pub is_quick_play_singleplayer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Whether the instance is being launched to a multi-player world
    pub is_quick_play_multiplayer: Option<bool>,
    ///  Whether the instance is being launched to a realms world
    pub is_quick_play_realms: Option<bool>,
}
impl FeatureRule {
    fn valid_with_state(&self, state: &Self) -> bool {
        (self.is_demo_user.is_none() || self.is_demo_user == state.is_demo_user)
            && (self.is_demo_user.is_none() || self.is_demo_user == state.is_demo_user)
            && (self.has_custom_resolution.is_none()
                || self.has_custom_resolution == state.has_custom_resolution)
            && (self.has_quick_plays_support.is_none()
                || self.has_quick_plays_support == state.has_quick_plays_support)
            && (self.is_quick_play_singleplayer.is_none()
                || self.is_quick_play_singleplayer == state.is_quick_play_singleplayer)
            && (self.is_quick_play_multiplayer.is_none()
                || self.is_quick_play_multiplayer == state.is_quick_play_multiplayer)
            && (self.is_quick_play_realms.is_none()
                || self.is_quick_play_realms == state.is_quick_play_realms)
    }
}
#[derive(Deserialize, Debug, Eq, PartialEq, Hash, Clone)]
#[serde(rename_all = "kebab-case")]
/// An enum representing the different types of operating systems
pub enum Os {
    /// MacOS (x86)
    Osx,
    /// M1-Based Macs
    OsxArm64,
    /// Windows (x86)
    Windows,
    /// Windows ARM
    WindowsArm64,
    /// Linux (x86) and its derivatives
    Linux,
    /// Linux ARM 64
    LinuxArm64,
    /// Linux ARM 32
    LinuxArm32,
    /// The OS is unknown
    Unknown,
}
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
/// The action a rule can follow
pub enum RuleAction {
    /// The rule's status allows something to be done
    Allow,
    /// The rule's status disallows something to be done
    Disallow,
}
#[derive(Deserialize, Debug, Clone)]
/// Data of all game versions of Minecraft
pub struct VersionManifest {
    /// A list of game versions of Minecraft
    pub versions: Vec<Version>,
}
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
/// A game version of Minecraft
pub struct Version {
    /// A unique identifier of the version
    pub id: String,
    #[serde(rename = "type")]
    /// The release type of the version
    pub type_: VersionType,
    /// A link to additional information about the version
    pub url: String,
    /// The SHA1 hash of the additional information about the version
    pub sha1: String,
}
