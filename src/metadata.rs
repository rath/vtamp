use crate::{
    import_config::Config,
    llm::Provider,
    platform::Paths,
    subprocess::Cancel,
    youtube::{self, Source},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub title: String,
    pub artist: String,
    pub method: String,
    #[serde(default)]
    pub warning: Option<String>,
}
impl Metadata {
    /// Add a warning without losing an earlier one.
    pub fn warn(&mut self, text: String) {
        self.warning = Some(match self.warning.take() {
            Some(earlier) => format!("{earlier}; {text}"),
            None => text,
        });
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Extracted {
    title: Option<String>,
    artists: Vec<String>,
    version: Option<String>,
    evidence: Value,
}
pub fn rules(source: &Source) -> Metadata {
    use std::sync::LazyLock;
    static QUOTED: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"^([^\[\]'\"‘“]{1,120})\s+['\"‘“]([^'\"’”]+)['\"’”]\s*$"#).unwrap()
    });
    static DIVIDED: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^([^\[\]]{1,120})\s+[-–—]\s+(.+)$").unwrap());
    let mut result = Metadata {
        title: source.original_title.clone(),
        artist: "Unknown artist".into(),
        method: "original".into(),
        warning: None,
    };
    if let Some(c) = QUOTED
        .captures(&source.original_title)
        .or_else(|| DIVIDED.captures(&source.original_title))
    {
        let artist = c[1].trim();
        // Avoid inferring performers from prose or common descriptive prefixes.
        if !["부르는", "원곡", "cover by", "playlist", "직캠", "concert"]
            .iter()
            .any(|s| artist.to_lowercase().contains(s))
            && artist.split_whitespace().count() <= 8
        {
            result.title = youtube::clean(&c[2]);
            result.artist = artist
                .split(" + ")
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(", ");
            result.method = "rules".into();
        }
    }
    if let Some(t) = &source.music_title {
        result.title = t.clone();
        result.method = "music_metadata".into();
    }
    if let Some(a) = &source.music_artist {
        result.artist = a.clone();
        result.method = "music_metadata".into();
    }
    result
}
pub fn resolve(source: &Source, config: &Config, paths: &Paths, stop: &Cancel) -> Metadata {
    let mut result = rules(source);
    if config.llm.provider != Provider::None
        && (source.music_title.is_none() || source.music_artist.is_none())
    {
        match infer(source, config, paths, stop) {
            Ok(v) => {
                if source.music_title.is_none()
                    && let Some(t) = v.title.filter(|s| !s.trim().is_empty())
                {
                    result.title = youtube::clean(&t);
                    result.method = "llm".into();
                }
                if source.music_artist.is_none() && !v.artists.is_empty() {
                    result.artist = v
                        .artists
                        .iter()
                        .map(|s| youtube::clean(s))
                        .collect::<Vec<_>>()
                        .join(", ");
                    result.method = "llm".into();
                }
                if source.music_title.is_none()
                    && let Some(version) = v.version.filter(|s| !s.trim().is_empty())
                    && !result
                        .title
                        .to_lowercase()
                        .contains(&version.to_lowercase())
                {
                    result.title = format!("{} ({})", result.title, youtube::clean(&version));
                }
            }
            Err(e) => result.warning = Some(format!("LLM unavailable; used built-in rules: {e:#}")),
        }
    }
    result
}
fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
    "title":{"type":["string","null"]},"artists":{"type":"array","items":{"type":"string"}},"version":{"type":["string","null"]},
    "evidence":{"type":"object","additionalProperties":false,"properties":{"title":{"type":["string","null"]},"artists":{"type":["string","null"]}},"required":["title","artists"]}},"required":["title","artists","version","evidence"]})
}
fn infer(source: &Source, config: &Config, paths: &Paths, stop: &Cancel) -> Result<Extracted> {
    let instruction = include_str!("prompts/music_metadata.txt");
    let data = serde_json::to_string(source)?;
    let response = crate::llm::request(&config.llm, paths, instruction, &data, &schema(), stop)?;
    let mut value: Extracted =
        serde_json::from_value(response).context("LLM returned invalid metadata JSON")?;
    if value.artists.len() > 20
        || value.artists.iter().any(|s| s.len() > 512)
        || value.title.as_ref().is_some_and(|s| s.len() > 2048)
    {
        bail!("LLM metadata exceeds field limits");
    }
    // Evidence must exist in the supplied material. It is a guard, not a confidence score.
    let evidence_fields = [
        source.original_title.as_str(),
        source.description.as_str(),
        source.music_title.as_deref().unwrap_or(""),
        source.music_artist.as_deref().unwrap_or(""),
    ];
    for key in ["title", "artists"] {
        if let Some(s) = value.evidence[key].as_str()
            && (s.trim().is_empty() || !evidence_fields.iter().any(|field| field.contains(s)))
        {
            bail!("LLM cited evidence absent from the supplied metadata");
        }
    }
    if value.evidence["title"].as_str().is_none() {
        value.title = None;
        value.version = None;
    }
    if value.evidence["artists"].as_str().is_none() {
        value.artists.clear();
    }
    if value.version.as_ref().is_some_and(|v| v.len() > 256)
        || value.artists.iter().any(|v| v.trim().is_empty())
    {
        bail!("Invalid metadata fields");
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess;
    use std::{
        io::{Read, Write},
        time::Duration,
    };
    fn fixture() -> Source {
        Source {
            original_title: "Singer - Song (Live)".into(),
            ..Default::default()
        }
    }
    fn paths(dir: &std::path::Path) -> Paths {
        Paths {
            data: dir.into(),
            cache: dir.join("covers"),
            runtime: dir.join("run"),
        }
    }
    fn mock_api(responses: Vec<(u16, Value)>) -> (Config, std::thread::JoinHandle<Vec<Value>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, response) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut b = [0];
                    stream.read_exact(&mut b).unwrap();
                    bytes.push(b[0]);
                    if bytes.ends_with(b"\r\n\r\n") {
                        break bytes.len();
                    }
                    assert!(bytes.len() < 8192);
                };
                let header = String::from_utf8_lossy(&bytes).to_lowercase();
                assert!(header.starts_with("post /v1/chat/completions "));
                let len = header
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                bytes.resize(header_end + len, 0);
                stream.read_exact(&mut bytes[header_end..]).unwrap();
                requests.push(serde_json::from_slice(&bytes[header_end..]).unwrap());
                let body = serde_json::to_vec(&response).unwrap();
                write!(stream, "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        let config = Config {
            llm: crate::llm::Config {
                provider: Provider::Api,
                endpoint: Some(format!("http://{address}/v1")),
                model: Some("test-model".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        (config, thread)
    }
    fn completion(value: Value) -> Value {
        json!({"choices":[{"message":{"content":value.to_string()}}]})
    }
    #[test]
    fn api_schema_fallback_omits_default_effort_and_uses_only_supported_fields() {
        let output = json!({"title":"Song","artists":[],"version":"Live","evidence":{"title":"Song","artists":null}});
        let (config, server) = mock_api(vec![
            (400, json!({"error":"unsupported response_format"})),
            (200, completion(output)),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let metadata = resolve(
            &fixture(),
            &config,
            &paths(dir.path()),
            &subprocess::cancel(),
        );
        assert_eq!(metadata.title, "Song (Live)");
        assert_eq!(metadata.artist, "Singer");
        assert!(metadata.warning.is_none());
        let requests = server.join().unwrap();
        assert!(requests[0].get("response_format").is_some());
        assert!(requests[1].get("response_format").is_none());
        assert!(requests[0].get("reasoning_effort").is_none());
        assert_eq!(requests[0]["messages"][0]["role"], "system");
        assert_eq!(
            requests[0]["messages"][0]["content"],
            include_str!("prompts/music_metadata.txt")
        );
        assert_eq!(requests[0]["messages"][1]["role"], "user");
        assert_eq!(requests[0]["messages"], requests[1]["messages"]);
    }
    #[test]
    fn evidence_uses_individual_source_fields_including_structured_title() {
        let source = Source {
            original_title: "Upload".into(),
            description: "Performed by Artist".into(),
            music_title: Some("Structured Song".into()),
            ..Default::default()
        };
        for (evidence, accepted) in [
            ("Structured Song", true),
            ("Upload Performed by Artist", false),
        ] {
            let output = json!({"title":"Structured Song","artists":["Artist"],"version":null,"evidence":{"title":evidence,"artists":"Performed by Artist"}});
            let (config, server) = mock_api(vec![(200, completion(output))]);
            let dir = tempfile::tempdir().unwrap();
            let metadata = resolve(&source, &config, &paths(dir.path()), &subprocess::cancel());
            server.join().unwrap();
            assert_eq!(metadata.title, "Structured Song");
            assert_eq!(metadata.warning.is_none(), accepted);
            assert_eq!(
                metadata.artist,
                if accepted { "Artist" } else { "Unknown artist" }
            );
        }
    }
    #[test]
    fn invalid_json_or_evidence_falls_back_without_failing_import() {
        for output in [
            json!({"bogus":true}),
            json!({"title":"Invented","artists":["Someone"],"version":null,"evidence":{"title":"Not in input","artists":null}}),
        ] {
            let (config, server) = mock_api(vec![(200, completion(output))]);
            let dir = tempfile::tempdir().unwrap();
            let metadata = resolve(
                &fixture(),
                &config,
                &paths(dir.path()),
                &subprocess::cancel(),
            );
            server.join().unwrap();
            assert_eq!(metadata.title, "Song (Live)");
            assert_eq!(metadata.artist, "Singer");
            assert!(metadata.warning.is_some());
        }
    }
    #[test]
    fn api_request_can_be_cancelled_while_waiting_for_headers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = subprocess::cancel();
        let remote_stop = stop.clone();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            remote_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(150));
        });
        let config = Config {
            llm: crate::llm::Config {
                provider: Provider::Api,
                endpoint: Some(format!("http://{address}/v1")),
                model: Some("test".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        assert!(
            infer(&fixture(), &config, &paths(dir.path()), &stop)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }
    #[test]
    fn cli_providers_receive_isolated_json_only_requests() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("instruction.txt"),
            include_str!("prompts/music_metadata.txt"),
        )
        .unwrap();
        // The fake CLI is a Python script. On a fresh macOS runner the first launch of
        // /usr/bin/python3 goes through the Command Line Tools shim and can take seconds, so
        // pay that cost before the probe whose timeout the test exercises.
        assert!(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", "import json"])
                .status()
                .unwrap()
                .success()
        );
        for provider in [Provider::Codex, Provider::Claude] {
            let exe = dir.path().join("fake-cli");
            let is_codex = provider == Provider::Codex;
            let result = json!({"title":"Song","artists":["Singer"],"version":"Live","evidence":{"title":"Song","artists":"Singer"}});
            let output = if is_codex {
                result
            } else {
                json!({"structured_output":result})
            };
            let required = if is_codex {
                vec![
                    "--ephemeral",
                    "--ignore-user-config",
                    "--output-schema",
                    "read-only",
                    "approval_policy=\"never\"",
                ]
            } else {
                vec![
                    "--safe-mode",
                    "--json-schema",
                    "--tools",
                    "--strict-mcp-config",
                    "--no-session-persistence",
                    "--system-prompt",
                ]
            };
            std::fs::write(
                &exe,
                format!(
                    r#"#!/usr/bin/python3
import sys,json,os
a=sys.argv[1:]
required={}
if '--help' in a:
 print(' '.join(required));sys.exit(0)
assert all(v in a for v in required)
assert 'vtamp-llm-' in os.getcwd()
assert a[a.index('--model')+1]=='test'
assert '--effort' not in a
prompt=sys.stdin.read()
instruction=open(os.path.join(os.path.dirname(__file__),'instruction.txt'),encoding='utf-8').read()
if '--system-prompt' in a:
 assert a[a.index('--system-prompt')+1]==instruction
 assert instruction not in prompt
 assert prompt.startswith('Schema: ')
 assert 'Singer - Song' not in a[a.index('--system-prompt')+1]
 assert a[a.index('--tools')+1]==''
else:
 assert prompt.startswith(instruction+'\nSchema: ')
source=json.loads(prompt.rsplit('\nInput: ',1)[1])
assert source['original_title']=='Singer - Song (Live)'
print({})
"#,
                    serde_json::to_string(&required).unwrap(),
                    serde_json::to_string(&output.to_string()).unwrap()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
            let config = Config {
                llm: crate::llm::Config {
                    provider,
                    executable: Some(exe),
                    model: Some("test".into()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let metadata = resolve(
                &fixture(),
                &config,
                &paths(dir.path()),
                &subprocess::cancel(),
            );
            assert!(metadata.warning.is_none(), "{:?}", metadata.warning);
            assert_eq!(metadata.method, "llm");
            assert_eq!(metadata.title, "Song (Live)");
        }
    }
    #[test]
    fn korean_duet_and_prose() {
        let mut s = Source {
            original_title: "이승환 + 정준일 '어떻게 사랑이 그래요'".into(),
            channel_name: Some("이승환 LEE SEUNG HWAN".into()),
            ..Default::default()
        };
        let m = rules(&s);
        assert_eq!(m.title, "어떻게 사랑이 그래요");
        assert_eq!(m.artist, "이승환, 정준일");
        s.original_title = "정준일이 부르는 이승환의 어떻게 사랑이 그래요".into();
        assert_eq!(rules(&s).artist, "Unknown artist");
        s.music_artist = Some("정준일".into());
        assert_eq!(rules(&s).artist, "정준일");
    }
}
