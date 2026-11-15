//! Where a managed import came from. The provider decides the directory under
//! `imports/` and the key that names the import inside it.
use crate::{
    platform::Paths,
    youtube::{self, TimeRange},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum Source {
    /// Boxed so a local source does not carry the size of YouTube metadata.
    Youtube(Box<youtube::Source>),
    Local(Local),
}

/// A video file imported from this machine. The original stays where it was;
/// only its name is kept, never its directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Local {
    /// The first 32 hexadecimal digits of the original file's SHA-256.
    pub key: String,
    pub original_name: String,
}

impl Local {
    pub const KEY_LENGTH: usize = 32;

    pub fn valid_key(key: &str) -> bool {
        key.len() == Self::KEY_LENGTH
            && key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    fn validate(&self) -> Result<()> {
        ensure!(Self::valid_key(&self.key), "Invalid local import key");
        ensure!(
            !self.original_name.trim().is_empty()
                && self.original_name.len() <= 1024
                && !self
                    .original_name
                    .chars()
                    .any(|c| c.is_control() || c == '/'),
            "Invalid local import file name"
        );
        Ok(())
    }
}

impl Source {
    /// The directory name of this import, unique within its provider and
    /// across providers: a YouTube key starts with an 11-character video ID,
    /// a local key is 32 hexadecimal digits.
    pub fn key(&self) -> String {
        match self {
            Self::Youtube(source) => source.key(),
            Self::Local(source) => source.key.clone(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Youtube(source) => source.validate(),
            Self::Local(source) => source.validate(),
        }
    }

    /// The provider's directory under `imports/`.
    pub fn provider(&self) -> &'static str {
        match self {
            Self::Youtube(_) => youtube::PROVIDER,
            Self::Local(_) => LOCAL_PROVIDER,
        }
    }

    pub fn directory(&self, paths: &Paths) -> PathBuf {
        root(paths, self.provider()).join(self.key())
    }

    pub fn youtube(&self) -> Option<&youtube::Source> {
        match self {
            Self::Youtube(source) => Some(source.as_ref()),
            Self::Local(_) => None,
        }
    }

    pub fn local(&self) -> Option<&Local> {
        match self {
            Self::Youtube(_) => None,
            Self::Local(source) => Some(source),
        }
    }

    pub fn into_youtube(self) -> Option<youtube::Source> {
        match self {
            Self::Youtube(source) => Some(*source),
            Self::Local(_) => None,
        }
    }

    pub fn range(&self) -> Option<TimeRange> {
        self.youtube().and_then(|source| source.range)
    }

    /// The title the source itself carries, before any metadata cleanup.
    pub fn original_title(&self) -> &str {
        match self {
            Self::Youtube(source) => &source.original_title,
            Self::Local(source) => &source.original_name,
        }
    }

    pub fn album(&self) -> Option<&str> {
        self.youtube()
            .and_then(|source| source.music_album.as_deref())
    }
}

impl From<youtube::Source> for Source {
    fn from(source: youtube::Source) -> Self {
        Self::Youtube(Box::new(source))
    }
}

impl From<Local> for Source {
    fn from(source: Local) -> Self {
        Self::Local(source)
    }
}

pub const LOCAL_PROVIDER: &str = "local";

/// Every provider directory, in the order scans register them.
pub const PROVIDERS: [&str; 2] = [youtube::PROVIDER, LOCAL_PROVIDER];

pub fn root(paths: &Paths, provider: &str) -> PathBuf {
    paths.data.join("imports").join(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local() -> Local {
        Local {
            key: "0123456789abcdef0123456789abcdef".into(),
            original_name: "concert.mkv".into(),
        }
    }

    #[test]
    fn providers_keep_their_wire_shapes() {
        let youtube = Source::from(youtube::Source {
            video_id: "lO3lG-qXU14".into(),
            video_url: youtube::video_url("lO3lG-qXU14"),
            original_title: "Title".into(),
            ..Default::default()
        });
        let value = serde_json::to_value(&youtube).unwrap();
        assert_eq!(value["provider"], "youtube");
        assert_eq!(value["video_id"], "lO3lG-qXU14");
        assert!(value.get("range").is_none());
        assert_eq!(serde_json::from_value::<Source>(value).unwrap(), youtube);

        let source = Source::from(local());
        assert_eq!(
            serde_json::to_value(&source).unwrap(),
            json!({
                "provider": "local",
                "key": "0123456789abcdef0123456789abcdef",
                "original_name": "concert.mkv"
            })
        );
        assert_eq!(
            serde_json::from_value::<Source>(serde_json::to_value(&source).unwrap()).unwrap(),
            source
        );
    }

    #[test]
    fn unknown_or_missing_providers_are_rejected() {
        for value in [
            json!({"provider": "vimeo", "video_id": "lO3lG-qXU14"}),
            json!({"video_id": "lO3lG-qXU14"}),
            json!({"provider": "local", "key": "0123456789abcdef0123456789abcdef"}),
        ] {
            assert!(serde_json::from_value::<Source>(value).is_err());
        }
    }

    #[test]
    fn keys_name_separate_provider_directories() {
        let paths = Paths {
            data: "/data".into(),
            runtime: "/run".into(),
            cache: "/cache".into(),
        };
        let source = Source::from(local());
        source.validate().unwrap();
        assert_eq!(
            source.directory(&paths),
            PathBuf::from("/data/imports/local/0123456789abcdef0123456789abcdef")
        );
        let youtube = Source::from(youtube::Source {
            video_id: "lO3lG-qXU14".into(),
            ..Default::default()
        });
        youtube.validate().unwrap();
        assert_eq!(
            youtube.directory(&paths),
            PathBuf::from("/data/imports/youtube/lO3lG-qXU14")
        );
    }

    #[test]
    fn local_identity_is_validated() {
        for key in [
            "",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdeg",
            "lO3lG-qXU14",
        ] {
            let mut source = local();
            source.key = key.into();
            assert!(Source::from(source).validate().is_err(), "{key}");
        }
        for name in ["", " ", "a/b.mkv", "a\nb.mkv"] {
            let mut source = local();
            source.original_name = name.into();
            assert!(Source::from(source).validate().is_err(), "{name:?}");
        }
    }
}
