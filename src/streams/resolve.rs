//! Resolve `radio.bsod.kr` fixed URLs to broadcaster stream URLs.
//!
//! The site's `/stream` endpoint is a redirecting index, but the custom domain
//! sits behind a Cloudflare browser challenge that AVPlayer cannot pass. Resolve
//! the same channel through the broadcaster's public API instead, so playback
//! never touches the challenged host. The static map and provider rules mirror
//! the site's worker sources as of 2026-11-03.

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_TEXT: usize = 1024 * 1024;

/// A registered URL that is a redirecting `radio.bsod.kr` channel.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A direct URL for the native player.
    Direct(String),
    /// A fixed URL whose station or channel is not in the map.
    Unsupported(String),
}

#[derive(Debug, PartialEq, Eq)]
struct FixedParams {
    station: Option<String>,
    channel: Option<String>,
    city: Option<String>,
    bora: bool,
}

/// Whether this URL is a `radio.bsod.kr` fixed channel URL.
pub(crate) fn needs_resolution(input: &str) -> bool {
    fixed_params(input).is_some()
}

/// Resolve a `radio.bsod.kr` fixed channel URL to a direct stream URL.
pub(crate) fn resolve_stream(input: &str) -> Result<Resolved> {
    let params = fixed_params(input).context("Not a radio.bsod.kr fixed stream URL")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Cannot start the stream URL resolver")?;
    runtime.block_on(resolve_params(&params))
}

fn fixed_params(input: &str) -> Option<FixedParams> {
    let url = url::Url::parse(input).ok()?;
    if !url.host_str().is_some_and(|host| {
        host.trim_end_matches('.')
            .eq_ignore_ascii_case("radio.bsod.kr")
    }) || !matches!(url.path(), "/stream" | "/stream/")
    {
        return None;
    }
    let mut station = None;
    let mut channel = None;
    let mut city = None;
    let mut bora = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "stn" => station = Some(value.into_owned()),
            "ch" => channel = Some(value.into_owned()),
            "city" => city = Some(value.into_owned()),
            "bora" => bora = value.as_ref() == "true",
            _ => {}
        }
    }
    Some(FixedParams {
        station,
        channel,
        city,
        bora,
    })
}

async fn resolve_params(params: &FixedParams) -> Result<Resolved> {
    let station = params.station.as_deref();
    let channel = params.channel.as_deref();
    let city = params.city.as_deref();
    let direct = match station {
        Some("kbs") => Some(kbs(channel, city, params.bora).await?),
        Some("mbc") => Some(mbc(channel, city, params.bora).await?),
        Some("sbs") => Some(sbs(channel, city, params.bora).await?),
        Some("ebs") => Some(ebs(channel).await?),
        Some("afn") => Some(afn(city).await?),
        Some("cbs") => cbs(channel, city, params.bora),
        Some("ifm") => Some(ifm(params.bora).to_owned()),
        _ => static_url(station.unwrap_or_default(), channel, city).map(str::to_owned),
    };
    let Some(direct) = direct else {
        let query = [
            station.map(|value| format!("stn={value}")),
            channel.map(|value| format!("ch={value}")),
            city.map(|value| format!("city={value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("&");
        return Ok(Resolved::Unsupported(format!(
            "radio.bsod.kr fixed URL ({query}) has no known direct stream; register its direct stream URL"
        )));
    };
    Ok(Resolved::Direct(crate::streams::validated_url(&direct)?))
}

async fn kbs(channel: Option<&str>, city: Option<&str>, bora: bool) -> Result<String> {
    let channel = channel.context("KBS channel is missing")?;
    let code = KBS_CHANNELS
        .iter()
        .find_map(|(name, code)| (*name == channel).then_some(*code))
        .with_context(|| format!("Unsupported KBS channel {channel}"))?;
    let prefix = match city {
        None => "",
        Some(city) => KBS_CITIES
            .iter()
            .find_map(|(name, prefix)| (*name == city).then_some(*prefix))
            .with_context(|| format!("Unsupported KBS city {city}"))?,
    };
    let endpoint =
        format!("https://cfpwwwapi.kbs.co.kr/api/v1/landing/live/channel_code/{prefix}{code}");
    let text = get_text(&endpoint, 64 * 1024).await?;
    kbs_from_json(&text, bora)
}

fn kbs_from_json(text: &str, bora: bool) -> Result<String> {
    let value: Value = serde_json::from_str(text).context("KBS returned invalid JSON")?;
    let items = value
        .get("channel_item")
        .and_then(Value::as_array)
        .or_else(|| value.pointer("/channel/item").and_then(Value::as_array))
        .context("KBS response has no channel_item")?;
    let index = usize::from(bora && items.len() > 1);
    items
        .get(index)
        .and_then(|item| item.get("service_url"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("KBS response has no service_url")
}

async fn mbc(channel: Option<&str>, city: Option<&str>, bora: bool) -> Result<String> {
    if let Some(city) = city {
        if city == "busan" {
            return mbc_busan(channel);
        }
        if let Some(url) = static_exact("mbc", channel, Some(city)) {
            return Ok(url.to_owned());
        }
    }
    if bora || channel == Some("bora") {
        let text = get_text(
            "https://sminiplay.imbc.com/boraplay.ashx?agent=webapp&callback=__boraplay",
            64 * 1024,
        )
        .await?;
        return bora_url(&text).context("MBC 보이는 라디오 response has no BoraURL");
    }
    let channel = channel.context("MBC channel is missing")?;
    let code = match channel {
        "sfm" => "sfm",
        "fm4u" => "mfm",
        "chm" => "chm",
        _ => bail!("Unsupported MBC channel {channel}"),
    };
    let text = get_text(
        &format!("https://sminiplay.imbc.com/aacplay.ashx?agent=webapp&channel={code}"),
        64 * 1024,
    )
    .await?;
    Ok(text.trim().to_owned())
}

fn mbc_busan(channel: Option<&str>) -> Result<String> {
    let stream = match channel {
        Some("sfm") => "BusanMBC_AM_onairstream",
        Some("fm4u") => "BusanMBC_FM_onairstream",
        Some(channel) => bail!("Unsupported busan MBC channel {channel}"),
        None => bail!("busan MBC channel is missing"),
    };
    Ok(format!(
        "https://brook.bsmbc.com/live/{stream}.{}/playlist.m3u8",
        busan_datecode(SystemTime::now())
    ))
}

async fn sbs(channel: Option<&str>, city: Option<&str>, bora: bool) -> Result<String> {
    if let Some(city) = city
        && let Some(url) = static_exact("sbs", channel, Some(city))
    {
        return Ok(url.to_owned());
    }
    let channel = channel.context("SBS channel is missing")?;
    let endpoint = match (channel, bora) {
        ("lovefm", false) => {
            "https://apis.sbs.co.kr/play-api/1.0/livestream/lovepc/lovefm?protocol=hls&ssl=Y"
        }
        ("lovefm", true) => {
            "https://apis.sbs.co.kr/play-api/1.0/livestream/visuallove/visuallove4?protocol=hls&ssl=Y"
        }
        ("powerfm", false) => {
            "https://apis.sbs.co.kr/play-api/1.0/livestream/powerpc/powerfm?protocol=hls&ssl=Y"
        }
        ("powerfm", true) => {
            "https://apis.sbs.co.kr/play-api/1.0/livestream/visualpower/visualpower4?protocol=hls&ssl=Y"
        }
        ("dmb", _) => {
            "https://apis.sbs.co.kr/play-api/1.0/livestream/sbsdmbpc/sbsdmb?protocol=hls&ssl=Y"
        }
        _ => bail!("Unsupported SBS channel {channel}"),
    };
    let text = get_text(endpoint, 64 * 1024).await?;
    Ok(text.trim().to_owned())
}

async fn ebs(channel: Option<&str>) -> Result<String> {
    match channel {
        Some("fm") => {
            let url = match get_text(
                "https://www.ebs.co.kr/onair?channelCodeString=radio",
                MAX_TEXT,
            )
            .await
            {
                Ok(html) => ebs_fm_url(&html).map(str::to_owned),
                Err(_) => None,
            };
            Ok(url.unwrap_or_else(|| EBS_FM_FALLBACK.to_owned()))
        }
        Some("bandi") => Ok("https://ebsonair.ebs.co.kr/cloud1/iradio/playlist.m3u8".to_owned()),
        Some(channel) => bail!("Unsupported EBS channel {channel}"),
        None => bail!("EBS channel is missing"),
    }
}

fn ebs_fm_url(html: &str) -> Option<&str> {
    let start = html.find("src: '")? + "src: '".len();
    let rest = &html[start..];
    let end = rest.find('\'')?;
    rest.get(..end)
        .filter(|url| url.starts_with("https://liveotu.ebs.co.kr/"))
}

fn cbs(channel: Option<&str>, city: Option<&str>, bora: bool) -> Option<String> {
    if let Some(city) = city
        && let Some(url) = static_exact("cbs", channel, Some(city))
    {
        return Some(url.to_owned());
    }
    if let Some(url) = static_exact("cbs", channel, None) {
        return Some(url.to_owned());
    }
    (channel == Some("sfm")).then(|| {
        if bora {
            "https://event-aac.cbs.co.kr/fm/_definst_/fm.stream/playlist.m3u8"
        } else {
            "https://m-aac.cbs.co.kr/mweb_cbs981/_definst_/cbs981.stream/playlist.m3u8"
        }
        .to_owned()
    })
}

fn ifm(bora: bool) -> &'static str {
    if bora {
        "https://stream.ifm.kr/live/bora1/playlist.m3u8"
    } else {
        "https://stream.ifm.kr/live/aod1/playlist.m3u8"
    }
}

async fn afn(city: Option<&str>) -> Result<String> {
    let (station, fallback, file) = match city {
        Some("humphreys") => (
            "AFNP_OSN",
            "https://14613.live.streamtheworld.com/AFNP_OSNAAC.aac",
            "AFNP_OSNAAC.aac",
        ),
        Some("daegu") => (
            "AFNP_DGU",
            "https://14943.live.streamtheworld.com/AFNP_DGUAAC.aac",
            "AFNP_DGUAAC.aac",
        ),
        Some("kunsan") => (
            "AFNP_KSN",
            "https://21633.live.streamtheworld.com/AFNP_KSNAAC.aac",
            "AFNP_KSNAAC.aac",
        ),
        Some(city) => bail!("Unsupported AFN city {city}"),
        None => bail!("AFN city is missing"),
    };
    let endpoint = format!(
        "https://playerservices.streamtheworld.com/api/livestream?station={station}&transports=http%2Chls&version=1.10"
    );
    let text = get_text(&endpoint, 64 * 1024).await?;
    Ok(last_ip(&text)
        .map(|host| format!("https://{host}/{file}"))
        .unwrap_or_else(|| fallback.to_owned()))
}

fn last_ip(text: &str) -> Option<&str> {
    let mut found = None;
    let mut rest = text;
    while let Some(start) = rest.find("<ip>") {
        rest = &rest[start + "<ip>".len()..];
        let Some(end) = rest.find("</ip>") else {
            break;
        };
        found = rest.get(..end);
        rest = &rest[end + "</ip>".len()..];
    }
    found
}

fn bora_url(text: &str) -> Option<String> {
    let key = text.find("\"BoraURL\"")?;
    let rest = &text[key + "\"BoraURL\"".len()..];
    let start = rest.find('"')? + 1;
    let rest = &rest[start..];
    let end = rest.find('"')?;
    Some(rest.get(..end)?.replace("\\/", "/"))
}

fn busan_datecode(now: SystemTime) -> String {
    let seconds = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 + 9 * 60 * 60;
    let days = seconds.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{:02}{month:02}{day:02}", year.rem_euclid(100))
        .chars()
        .map(|c| match c {
            '0' => 'q',
            '1' => 'h',
            '2' => 's',
            '3' => 'a',
            '4' => 'g',
            '5' => 'b',
            '6' => 'j',
            '7' => 'c',
            '8' => 'p',
            '9' => 'd',
            other => other,
        })
        .collect()
}

/// Convert days since 1970-01-01 to a Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

async fn get_text(url: &str, limit: usize) -> Result<String> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10));
    // MBC's IIS endpoint only offers a TLS 1.2 CBC suite, which rustls omits.
    #[cfg(target_os = "macos")]
    let builder = builder.use_native_tls();
    let client = builder
        .build()
        .context("Cannot create stream resolver client")?;
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Cannot fetch {url}"))?;
    let status = response.status();
    ensure!(status.is_success(), "{url} returned HTTP {status}");
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("Cannot read {url}"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "{url} response exceeds {limit} bytes"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// Static entries mirror the site's worker `functions/stream/url` tables. Dynamic
// providers are handled above before this lookup.
type Mapping = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

const KBS_CHANNELS: &[(&str, &str)] = &[
    ("1radio", "21"),
    ("2radio", "22"),
    ("3radio", "23"),
    ("1fm", "24"),
    ("2fm", "25"),
    ("hanminjok", "26"),
];

const KBS_CITIES: &[(&str, &str)] = &[
    ("busan", "10_"),
    ("ulsan", "11_"),
    ("changwon", "20_"),
    ("jinju", "21_"),
    ("daegu", "30_"),
    ("andong", "31_"),
    ("pohang", "32_"),
    ("gwangju", "40_"),
    ("mokpo", "41_"),
    ("suncheon", "43_"),
    ("jeonju", "50_"),
    ("daejeon", "60_"),
    ("cheongju", "70_"),
    ("chungju", "71_"),
    ("chuncheon", "80_"),
    ("gangneung", "81_"),
    ("wonju", "82_"),
    ("jeju", "90_"),
];

const EBS_FM_FALLBACK: &str = "https://ebsonair.ebs.co.kr/fmradiofamilypc/familypc1m/playlist.m3u8";

fn static_exact(station: &str, channel: Option<&str>, city: Option<&str>) -> Option<&'static str> {
    STATIC
        .iter()
        .find(|(s, c, y, _)| *s == station && *c == channel && *y == city)
        .map(|(_, _, _, url)| *url)
}

fn static_url(station: &str, channel: Option<&str>, city: Option<&str>) -> Option<&'static str> {
    if let Some(channel) = channel {
        if let Some(city) = city
            && let Some(url) = static_exact(station, Some(channel), Some(city))
        {
            return Some(url);
        }
        if let Some(url) = static_exact(station, Some(channel), None) {
            return Some(url);
        }
        if let Some(url) = STATIC
            .iter()
            .find(|(s, c, _, _)| *s == station && *c == Some(channel))
            .map(|(_, _, _, url)| *url)
        {
            return Some(url);
        }
    }
    if let Some(city) = city
        && let Some(url) = STATIC
            .iter()
            .find(|(s, _, y, _)| *s == station && *y == Some(city))
            .map(|(_, _, _, url)| *url)
    {
        return Some(url);
    }
    static_exact(station, None, None)
}

const STATIC: &[Mapping] = &[
    // MBC regional stations. The Seoul channels and Busan are dynamic.
    (
        "mbc",
        Some("sfm"),
        Some("ulsan"),
        "https://5ddfd163bd00d.streamlock.net/STDFM/STDFM/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("changwon"),
        "https://wowza.mbcgn.kr/MBCFM/TV2.stream/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("daegu"),
        "https://5ee1ec6f32118.streamlock.net/amradio/am/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("andong"),
        "https://live.andongmbc.co.kr/live/amlive/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("pohang"),
        "http://stream.yubinet.com:1935/live/_definst_/Radio_Am/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("gwangju"),
        "https://media.kjmbc.co.kr/hls/amlive/GWANGJU-MBC-AM/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("mokpo"),
        "https://vod.mpmbc.co.kr/live/encoder-am/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("yeosu"),
        "https://5c3639aa99149.streamlock.net/표준FM/표준FM/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("jeonju"),
        "https://live.jmbc.co.kr/live/am_live.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("daejeon"),
        "https://ns1.tjmbc.co.kr/live_am/live_am.stream/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("cheongju"),
        "https://mbccbp.coreit.co.kr/radio_stfm/myStream.sdp/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("chuncheon"),
        "https://stream.chmbc.co.kr/radio1/fm1_aac/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("wonju"),
        "http://live.wjmbc.co.kr:1935/fm2/fm2/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("gangneung"),
        "http://123.254.72.24:1935/amlive/livestream/playlist.m3u8",
    ),
    (
        "mbc",
        Some("sfm"),
        Some("jeju"),
        "https://onairfm.jejumbc.com/hls/onairfmjejumbc/index.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("ulsan"),
        "https://5ddfd163bd00d.streamlock.net/FM4U/FM4U/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("changwon"),
        "https://wowza.mbcgn.kr/MBCFM4U/TV3.stream/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("daegu"),
        "https://5ee1ec6f32118.streamlock.net/fmradio/fm/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("andong"),
        "https://live.andongmbc.co.kr/live/fmlive/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("pohang"),
        "http://stream.yubinet.com:1935/live/_definst_/Radio_Fm/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("gwangju"),
        "https://media.kjmbc.co.kr/hls/fmlive/GWANGJU-MBC-FM/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("mokpo"),
        "https://vod.mpmbc.co.kr/live/encoder-fm/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("yeosu"),
        "https://5c3639aa99149.streamlock.net/FM4U/FM4U/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("jeonju"),
        "https://live.jmbc.co.kr/live/fm_live.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("daejeon"),
        "https://ns1.tjmbc.co.kr/live_fm/live_fm.stream/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("cheongju"),
        "https://mbccbp.coreit.co.kr/radio_fm/myStream.sdp/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("chuncheon"),
        "https://stream.chmbc.co.kr/live_radio2/fm1/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("wonju"),
        "http://live.wjmbc.co.kr:1935/fm989/fm989/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("gangneung"),
        "http://123.254.72.24:1935/fmlive/livestream/playlist.m3u8",
    ),
    (
        "mbc",
        Some("fm4u"),
        Some("jeju"),
        "https://onairsfm.jejumbc.com/hls/onairsfmjejumbc/1_2/index.m3u8",
    ),
    // SBS regional stations. The Seoul channels are dynamic.
    (
        "sbs",
        Some("lovefm"),
        Some("busan"),
        "https://stream1.knn.co.kr/hls/b3y26uu6471k8tes9w7h_lfm/index.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("busan"),
        "https://stream1.knn.co.kr/hls/lb9ezl87d37uu0vy65bb_pfm/index.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("ulsan"),
        "https://stream.ubc.co.kr/hls/ubcfmstream/index.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("daegu"),
        "http://203.251.91.122:1935/on-air-Backup/fm/playlist.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("gwangju"),
        "https://vod.ikbc.co.kr/KBCFM/kbcra_aac/playlist.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("jeonju"),
        "http://61.85.197.53:1935/jtv_radio/myStream/playlist.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("daejeon"),
        "https://vod.tjb.co.kr/radiolive/_definst_/radio_64k/playlist.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("cheongju"),
        "https://wowza1.cjb.co.kr/live/cjbradio/playlist.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("chuncheon"),
        "http://61.82.49.4:1935/fm/_definst_/myStream/playlist.m3u8",
    ),
    (
        "sbs",
        Some("powerfm"),
        Some("jeju"),
        "http://123.140.197.22/stream/2/play.m3u8",
    ),
    // CBS. The Seoul 표준FM channel is dynamic; all others are static.
    (
        "cbs",
        Some("sfm"),
        Some("busan"),
        "https://m-aac.cbs.co.kr/busan939/_definst_/busan939.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("ulsan"),
        "https://m-aac.cbs.co.kr/ulsan/_definst_/ulsan.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("gyeongnam"),
        "https://m-aac.cbs.co.kr/gyeongnam/_definst_/gyeongnam.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("daegu"),
        "https://m-aac.cbs.co.kr/daegu/_definst_/daegu.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("pohang"),
        "https://m-aac.cbs.co.kr/pohang/_definst_/pohang.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("gwangju"),
        "https://m-aac.cbs.co.kr/gwangju/_definst_/gwangju.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("jeonnam"),
        "https://m-aac.cbs.co.kr/jeonnam/_definst_/jeonnam.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("jeonbuk"),
        "https://m-aac.cbs.co.kr/jeonbuk/_definst_/jeonbuk.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("daejeon"),
        "https://m-aac.cbs.co.kr/daejeon/_definst_/daejeon.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("cheongju"),
        "https://m-aac.cbs.co.kr/cheongju/_definst_/cheongju.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("chuncheon"),
        "https://m-aac.cbs.co.kr/chuncheon/_definst_/chuncheon.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("youngdong"),
        "https://m-aac.cbs.co.kr/youngdong/_definst_/youngdong.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("sfm"),
        Some("jeju"),
        "https://m-aac.cbs.co.kr/jeju/_definst_/jeju.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("mfm"),
        None,
        "https://m-aac.cbs.co.kr/mweb_cbs939/_definst_/cbs939.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("mfm"),
        Some("busan"),
        "https://m-aac.cbs.co.kr/busan981/_definst_/busan981.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("mfm"),
        Some("daegu"),
        "https://m-aac.cbs.co.kr/daegu939/_definst_/daegu939.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("mfm"),
        Some("gwangju"),
        "https://m-aac.cbs.co.kr/gwangju939/_definst_/gwangju939.stream/playlist.m3u8",
    ),
    (
        "cbs",
        Some("joy4u"),
        None,
        "https://m-aac.cbs.co.kr/mweb_cbscmc/_definst_/cbscmc.stream/playlist.m3u8",
    ),
    // FEBC.
    (
        "febc",
        None,
        None,
        "http://mlive2.febc.net:1935/live/seoulfm/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("busan"),
        "http://mlive2.febc.net:1935/live/bsfebc/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("ulsan"),
        "http://mlive2.febc.net:1935/live/uslive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("changwon"),
        "http://mlive2.febc.net:1935/live/cwlive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("daegu"),
        "https://mlive3.febc.net/live5/daegulive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("pohang"),
        "http://mlive2.febc.net:1935/live/phlive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("gwangju"),
        "http://mlive2.febc.net:1935/live/gjlive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("mokpo"),
        "https://mlive3.febc.net/live5/mplive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("jeonnam"),
        "http://mlive2.febc.net:1935/live/jndblive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("jeonbuk"),
        "http://mlive2.febc.net:1935/live/jblive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("daejeon"),
        "http://mlive2.febc.net:1935/live/djlive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("gangwon"),
        "https://mlive3.febc.net/live5/ydlive/playlist.m3u8",
    ),
    (
        "febc",
        None,
        Some("jeju"),
        "http://mlive2.febc.net:1935/live/jejufm/playlist.m3u8",
    ),
    // BBS.
    (
        "bbs",
        None,
        None,
        "https://bbslive.clouducs.com/bbsradio-mlive/radio.stream/playlist.m3u8",
    ),
    (
        "bbs",
        None,
        Some("busan"),
        "http://live.cdn.smilecdn.com:1935/bbsfm_live/live.sdp/playlist.m3u8",
    ),
    (
        "bbs",
        None,
        Some("daegu"),
        "https://bbslive.goldenday.kr:446/hls/dgbbs.m3u8",
    ),
    (
        "bbs",
        None,
        Some("gwangju"),
        "http://live.cdn.smilecdn.com:1935/kjbbs1_live/live/playlist.m3u8",
    ),
    (
        "bbs",
        None,
        Some("jeju"),
        "http://kdbmobile.clouducs.com:1935/jeju/jeju.stream_aac/playlist.m3u8",
    ),
    // CPBC.
    (
        "cpbc",
        None,
        None,
        "http://onair1.cpbc.co.kr:1935/live/_definst_/Instreamer.stream_aac/playlist.m3u8",
    ),
    (
        "cpbc",
        None,
        Some("busan"),
        "http://pbcradio.dynamicsmart.com:1935/radio/bscpbc-radio/index.m3u8",
    ),
    (
        "cpbc",
        None,
        Some("daegu"),
        "http://live.dgcpbc.co.kr/dgcpbclive/livestream/playlist.m3u8",
    ),
    (
        "cpbc",
        None,
        Some("gwangju"),
        "http://pbcradio.dynamicsmart.com:1935/radio/kjpbc2/index.m3u8",
    ),
    // WBS.
    ("wbs", None, None, "https://wbsradio.kr/wbs-seoul"),
    ("wbs", None, Some("busan"), "https://wbsradio2.kr/wbs-b"),
    ("wbs", None, Some("daegu"), "https://wbsradio2.kr/wbs-d"),
    ("wbs", None, Some("gwangju"), "https://wbsradio2.kr/wbs-g"),
    ("wbs", None, Some("jeonbuk"), "https://wbsradio2.kr/wbs-j"),
    // 국악방송.
    (
        "kugak",
        None,
        None,
        "https://mgugaklive.nowcdn.co.kr/gugakradio/gugakradio.stream/playlist.m3u8",
    ),
    (
        "kugak",
        None,
        Some("gwangju"),
        "https://mgugaklive.nowcdn.co.kr/gugakgwangju/Glive.stream/playlist.m3u8",
    ),
    (
        "kugak",
        None,
        Some("daejeon"),
        "https://mgugaklive.nowcdn.co.kr/gugakdaejeon/Daejeonradio.stream/playlist.m3u8",
    ),
    // TBS.
    (
        "tbs",
        Some("fm"),
        None,
        "https://cdnfm.tbs.seoul.kr/tbs/_definst_/tbs_fm_web_360.smil/playlist.m3u8",
    ),
    (
        "tbs",
        Some("efm"),
        None,
        "https://cdnefm.tbs.seoul.kr/tbs/_definst_/tbs_efm_web_360.smil/playlist.m3u8",
    ),
    // TBN.
    (
        "tbn",
        None,
        None,
        "https://www.tbn.it.kr/gyeongin/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("busan"),
        "https://www.tbn.it.kr/busan/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("ulsan"),
        "https://www.tbn.it.kr/ulsan/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("gyeongnam"),
        "https://www.tbn.it.kr/gyeongnam/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("daegu"),
        "https://www.tbn.it.kr/daegu/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("gyeongbuk"),
        "https://www.tbn.it.kr/kyungbuk/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("gwangju"),
        "https://www.tbn.it.kr/gwangju/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("jeonbuk"),
        "https://www.tbn.it.kr/jeonbuk/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("daejeon"),
        "https://www.tbn.it.kr/daejeon/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("chungbuk"),
        "https://www.tbn.it.kr/chungbuk/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("chungnam"),
        "https://www.tbn.it.kr/chungnam/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("gangwon"),
        "https://www.tbn.it.kr/gangwon/myStream/playlist.m3u8",
    ),
    (
        "tbn",
        None,
        Some("jeju"),
        "https://www.tbn.it.kr/jeju/myStream/playlist.m3u8",
    ),
    // Other national and regional stations.
    (
        "ytn",
        None,
        None,
        "https://radiolive.ytn.co.kr/radio/_definst_/20211118_fmlive/playlist.m3u8",
    ),
    (
        "obs",
        None,
        None,
        "https://vod3.obs.co.kr:444/live/obsstream1/radio.stream/playlist.m3u8",
    ),
    (
        "kookbang",
        None,
        None,
        "https://mediaworks.dema.mil.kr/live_edge/audio.sdp/playlist.m3u8",
    ),
    (
        "arirang",
        None,
        None,
        "https://amdlive-ch03-ctnd-com.akamaized.net/arirang_3ch/smil:arirang_3ch.smil/playlist.m3u8",
    ),
    (
        "befm",
        None,
        None,
        "https://live.cdn.smilecdn.com/befm905_live/live/playlist.m3u8",
    ),
    ("ggn", None, None, "http://media.ggn.or.kr:8080/live"),
    // Community stations keyed by channel.
    (
        "community",
        Some("keumkangfm"),
        None,
        "http://icecast.radios.kr:8000/keumkangfm",
    ),
    (
        "community",
        Some("yeongjufm"),
        None,
        "http://icecast.radios.kr:8000/yeongjufm",
    ),
    (
        "community",
        Some("gwanakfm"),
        None,
        "http://icecast.radios.kr:8000/gwanakfm",
    ),
    (
        "community",
        Some("kjfm"),
        None,
        "http://radiostream.communityradio.kr/live",
    ),
    (
        "community",
        Some("mapofm"),
        None,
        "http://onair.mapofm.net/mapofm",
    ),
    (
        "community",
        Some("seongnamfm"),
        None,
        "http://onair.mapofm.net/seongnamfm",
    ),
    (
        "community",
        Some("scnfm"),
        None,
        "http://onair.mapofm.net/scnfm",
    ),
    (
        "community",
        Some("obn"),
        None,
        "http://icecast.radios.kr:8000/obn",
    ),
    (
        "community",
        Some("sejongfm"),
        None,
        "http://icecast.radios.kr:8000/sejongfm",
    ),
    (
        "community",
        Some("crfm"),
        None,
        "http://icecast.radios.kr:8000/crfm",
    ),
    (
        "community",
        Some("hanbatfm"),
        None,
        "http://115.68.194.186:8000/hanbatfm",
    ),
    (
        "community",
        Some("sjfm"),
        None,
        "http://icecast.radios.kr:8000/sjfm",
    ),
    (
        "community",
        Some("namhaefm"),
        None,
        "http://icecast.radios.kr:8000/namhaefm",
    ),
    (
        "community",
        Some("sangjufm"),
        None,
        "http://icecast.radios.kr:8000/sangjufm",
    ),
    (
        "community",
        Some("sdmfm"),
        None,
        "http://icecast.radios.kr:8000/sdmfm",
    ),
    (
        "community",
        Some("sonefm"),
        None,
        "http://onair.mapofm.net/sonefm",
    ),
    (
        "community",
        Some("yeonjefm"),
        None,
        "http://icecast.radios.kr:8000/yeonjefm",
    ),
    (
        "community",
        Some("suncheonfm"),
        None,
        "http://icecast.radios.kr:8000/suncheonfm",
    ),
    (
        "community",
        Some("gurifm"),
        None,
        "http://icecast.radios.kr:8000/gurifm",
    ),
    (
        "community",
        Some("daegufm"),
        None,
        "http://icecast.radios.kr:8000/daegufm",
    ),
    (
        "community",
        Some("jcfm"),
        None,
        "http://radiostream.communityradio.kr/jcfm1",
    ),
    (
        "community",
        Some("dwfm"),
        None,
        "http://radiostream.communityradio.kr/dwfm1",
    ),
    (
        "community",
        Some("taebaekfm"),
        None,
        "http://icecast.radios.kr:8000/taebaekfm",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_fixed_bsod_streams() {
        assert!(needs_resolution(
            "https://radio.bsod.kr/stream?stn=mbc&ch=sfm"
        ));
        assert!(needs_resolution(
            "HTTPS://RADIO.BSOD.KR/stream/?stn=mbc&ch=sfm"
        ));
        for url in [
            "https://radio.bsod.kr/stream/player.html?stn=mbc&ch=sfm",
            "https://example.com/stream?stn=mbc&ch=sfm",
            "https://radio.bsod.kr/",
        ] {
            assert!(!needs_resolution(url), "{url}");
        }
    }

    #[test]
    fn parses_channel_parameters() {
        let params =
            fixed_params("https://radio.bsod.kr/stream?stn=mbc&ch=sfm&city=daejeon&bora=true")
                .unwrap();
        assert_eq!(
            params,
            FixedParams {
                station: Some("mbc".into()),
                channel: Some("sfm".into()),
                city: Some("daejeon".into()),
                bora: true,
            }
        );
    }

    #[test]
    fn static_mapping_prefers_exact_city() {
        assert_eq!(
            static_url("sbs", Some("powerfm"), Some("busan")),
            Some("https://stream1.knn.co.kr/hls/lb9ezl87d37uu0vy65bb_pfm/index.m3u8")
        );
        assert_eq!(
            static_url("cbs", Some("mfm"), Some("daegu")),
            Some("https://m-aac.cbs.co.kr/daegu939/_definst_/daegu939.stream/playlist.m3u8")
        );
        assert_eq!(
            static_url("cbs", Some("mfm"), Some("unknown")),
            Some("https://m-aac.cbs.co.kr/mweb_cbs939/_definst_/cbs939.stream/playlist.m3u8")
        );
        assert_eq!(
            static_url("ytn", None, None),
            Some("https://radiolive.ytn.co.kr/radio/_definst_/20211118_fmlive/playlist.m3u8")
        );
        assert_eq!(
            static_url("mbc", None, Some("daegu")),
            Some("https://5ee1ec6f32118.streamlock.net/amradio/am/playlist.m3u8")
        );
    }

    #[test]
    fn parses_kbs_payload() {
        let payload = r#"{"ret":0,"channel_item":[{"service_url":"https://1fm.example/a.m3u8"},{"service_url":"https://1fm.example/b.m3u8"}]}"#;
        assert_eq!(
            kbs_from_json(payload, false).unwrap(),
            "https://1fm.example/a.m3u8"
        );
        assert_eq!(
            kbs_from_json(payload, true).unwrap(),
            "https://1fm.example/b.m3u8"
        );
        assert!(kbs_from_json(r#"{"ret":0}"#, false).is_err());
    }

    #[test]
    fn extracts_mbc_bora_url() {
        let payload = "__boraplay({\n\t\"BoraURL\":\n\"https://example.com/bora.m3u8?_lsu_sa_=x\\/y\"\n\t}\n);";
        assert_eq!(
            bora_url(payload).as_deref(),
            Some("https://example.com/bora.m3u8?_lsu_sa_=x/y")
        );
        assert_eq!(bora_url("nope"), None);
    }

    #[test]
    fn extracts_ebs_fm_url() {
        let page = "window.player = { src: 'https://liveotu.ebs.co.kr/fm/fm.smil/playlist.m3u8?Policy=x', };";
        assert_eq!(
            ebs_fm_url(page),
            Some("https://liveotu.ebs.co.kr/fm/fm.smil/playlist.m3u8?Policy=x")
        );
        assert_eq!(ebs_fm_url("<html></html>"), None);
    }

    #[test]
    fn builds_busan_datecode_in_kst() {
        assert_eq!(busan_datecode(UNIX_EPOCH), "cqqhqh");
        // 2024-02-29 00:00 KST (2024-02-28 15:00 UTC).
        assert_eq!(
            busan_datecode(UNIX_EPOCH + Duration::from_secs(1_709_132_400)),
            "sgqssd"
        );
    }

    #[test]
    fn converts_civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn picks_last_afn_host() {
        let xml = "<ip>one</ip><ip>two</ip>";
        assert_eq!(last_ip(xml), Some("two"));
        assert_eq!(last_ip("none"), None);
    }
}
