// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Session descriptions the way Steam's voice servers expect and send them.
//!
//! Steam's server describes every participant as another `a=ssrc` block in one
//! audio m-section ("Plan B"). libwebrtc only speaks Unified Plan, where each
//! remote stream needs its own m-section, so Steam's descriptions are rewritten
//! before they are applied. That is safe because Steam never sees our answers,
//! only the acknowledgement of the version.
//!
//! Ported from vaporchat's `client/planB.ts` and `client/sdp.ts`
//! (https://github.com/vaporlens/vaporchat, MIT).

/// Steam's client forces this opus fmtp on its offers (in-band FEC, DTX).
const STEAM_OPUS_FMTP: &str = "minptime=10;useinbandfec=1;usedtx=1";
const MID_EXTENSION: &str = "urn:ietf:params:rtp-hdrext:sdes:mid";

/// Replaces the opus fmtp line of every m-section the way the official client does.
pub fn munge_opus_fmtp(sdp: &str) -> String {
    let opus: Vec<&str> = sdp
        .lines()
        .filter_map(|line| line.trim_end().strip_prefix("a=rtpmap:"))
        .filter_map(|rest| rest.split_once(' '))
        .filter(|(_, codec)| codec.to_ascii_lowercase().starts_with("opus/"))
        .map(|(pt, _)| pt)
        .collect();
    let mut out = String::with_capacity(sdp.len());
    for line in sdp.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        match body.strip_prefix("a=fmtp:").and_then(|rest| rest.split_once(' ')) {
            Some((pt, _)) if opus.contains(&pt) => {
                out.push_str("a=fmtp:");
                out.push_str(pt);
                out.push(' ');
                out.push_str(STEAM_OPUS_FMTP);
                out.push_str(ending);
            }
            _ => out.push_str(line),
        }
    }
    out
}

/// Steam names each remote participant's stream `Peer-<n>-<accountid>-...`.
pub fn account_id_from_stream_id(stream_id: &str) -> Option<u32> {
    let start = stream_id.find("Peer-")?;
    let mut parts = stream_id[start + 5..].splitn(3, '-');
    let n = parts.next()?;
    let account = parts.next()?;
    // Both numbers, and something after the account id.
    parts.next()?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    account.parse().ok()
}

/// Wraps an SDP as the JSON Steam exchanges: `{"type":"offer","sdp":"..."}`.
pub fn to_json(kind: &str, sdp: &str) -> String {
    serde_json::json!({ "type": kind, "sdp": sdp }).to_string()
}

/// The `type` and `sdp` of a description Steam sent as JSON.
pub fn from_json(json: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let kind = value.get("type")?.as_str()?.to_owned();
    let sdp = value.get("sdp").and_then(|s| s.as_str()).unwrap_or_default().to_owned();
    Some((kind, sdp))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteStream {
    ssrc: u32,
    stream_id: String,
    cname: String,
}

impl RemoteStream {
    /// `Self-...` is Steam echoing our own sending ssrc. Declaring it as a remote
    /// one would trigger ssrc collision handling and change our sending ssrc.
    fn is_own(&self) -> bool {
        self.stream_id.starts_with("Self-")
    }

    fn lines(&self) -> [String; 3] {
        let id = &self.stream_id;
        [
            format!("a=msid:{id} {id}-audio"),
            format!("a=ssrc:{} cname:{}", self.ssrc, self.cname),
            format!("a=ssrc:{} msid:{id} {id}-audio", self.ssrc),
        ]
    }
}

fn lines(section: &str) -> Vec<&str> {
    section
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect()
}

/// The session part and each m-section, split before every `m=` line.
fn split_sections(sdp: &str) -> (Vec<&str>, Vec<Vec<&str>>) {
    let mut session = Vec::new();
    let mut media: Vec<Vec<&str>> = Vec::new();
    for line in lines(sdp.trim()) {
        if line.starts_with("m=") {
            media.push(vec![line]);
        } else if let Some(section) = media.last_mut() {
            section.push(line);
        } else {
            session.push(line);
        }
    }
    (session, media)
}

fn parse_streams(section: &[&str]) -> Vec<RemoteStream> {
    let mut streams: Vec<RemoteStream> = Vec::new();
    for line in section {
        let Some(rest) = line.strip_prefix("a=ssrc:") else {
            continue;
        };
        let Some((ssrc, attr)) = rest.split_once(' ') else {
            continue;
        };
        let Ok(ssrc) = ssrc.parse::<u32>() else { continue };
        let Some((name, value)) = attr.split_once(':') else {
            continue;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let Some(value) = value.split_whitespace().next() else {
            continue;
        };
        let index = match streams.iter().position(|s| s.ssrc == ssrc) {
            Some(i) => i,
            None => {
                streams.push(RemoteStream {
                    ssrc,
                    stream_id: format!("ssrc-{ssrc}"),
                    cname: format!("ssrc-{ssrc}"),
                });
                streams.len() - 1
            }
        };
        let stream = &mut streams[index];
        match name {
            "msid" => stream.stream_id = value.to_owned(),
            "cname" => stream.cname = value.to_owned(),
            _ => {}
        }
    }
    streams
}

fn is_direction(line: &str) -> bool {
    ["a=sendrecv", "a=sendonly", "a=recvonly", "a=inactive"]
        .iter()
        .any(|d| line.starts_with(d))
}

/// Whether the session's BUNDLE group already lists more than one mid, i.e.
/// Steam sent one m-section per participant itself.
fn is_unified(session: &[&str]) -> bool {
    session
        .iter()
        .find_map(|l| l.strip_prefix("a=group:BUNDLE"))
        .is_some_and(|mids| mids.split_whitespace().count() > 1)
}

/// Rewrites Steam's Plan-B descriptions to Unified Plan. Stateful per peer
/// connection: Unified Plan never drops an m-section, so each remote stream
/// keeps the mid it got first, and streams that left turn inactive.
#[derive(Debug, Default)]
pub struct PlanBToUnifiedPlan {
    /// Remote ssrc → its m-section's mid, in the order they appeared.
    mids: Vec<(u32, String)>,
    /// What we send (Steam's `NotifyWebRTCSessionConnected`). Steam echoes it
    /// in its descriptions, not always as a `Self-...` stream.
    own_ssrc: Option<u32>,
}

impl PlanBToUnifiedPlan {
    pub fn new() -> Self {
        Self::default()
    }

    /// Our sending ssrc, never to be declared as a remote stream: that would
    /// trigger ssrc collision handling, change what we send, and Steam's
    /// relay would stop forwarding it.
    pub fn set_own_ssrc(&mut self, ssrc: u32) {
        self.own_ssrc = Some(ssrc);
    }

    /// Steam's description is already Unified Plan (seen 2026-11-03: one
    /// m-section per participant, `a=group:BUNDLE 0 m1`): keep it as it is,
    /// mids, BUNDLE and the mid header extension included, and only drop our
    /// own echoed stream (`Self-...` or our ssrc) so it isn't declared remote.
    fn pass_unified(&mut self, session: &[&str], media: &[Vec<&str>]) -> String {
        let mut out: Vec<String> = session.iter().map(|l| (*l).to_owned()).collect();
        for section in media {
            let streams = parse_streams(section);
            let own: Vec<u32> = streams
                .iter()
                .filter(|s| s.is_own() || Some(s.ssrc) == self.own_ssrc)
                .map(|s| s.ssrc)
                .collect();
            let is_own_line = |l: &str| {
                own.iter().any(|ssrc| l.starts_with(&format!("a=ssrc:{ssrc} ")))
                    || ["a=msid:Self-", "a=label:Self-"].iter().any(|p| l.starts_with(p))
            };
            let mid = section.iter().find_map(|l| l.strip_prefix("a=mid:"));
            if let Some(mid) = mid
                && let Some(peer) = streams.iter().find(|s| !own.contains(&s.ssrc))
                && !self.mids.iter().any(|(ssrc, _)| *ssrc == peer.ssrc)
            {
                self.mids.push((peer.ssrc, mid.to_owned()));
            }
            let kept: Vec<&str> = section.iter().copied().filter(|l| !is_own_line(l)).collect();
            let has_msid = kept.iter().any(|l| l.starts_with("a=msid:"));
            out.extend(kept.iter().map(|l| (*l).to_owned()));
            // Our own section has no remote stream left: "-" keeps libwebrtc from
            // inventing a default one (see the Plan-B path).
            if !has_msid {
                out.push("a=msid:-".to_owned());
            }
        }
        let mut sdp = out.join("\r\n");
        sdp.push_str("\r\n");
        sdp
    }

    /// The remote ssrc a converted m-section carries.
    pub fn ssrc_for_mid(&self, mid: &str) -> Option<u32> {
        self.mids.iter().find(|(_, m)| m == mid).map(|(ssrc, _)| *ssrc)
    }

    pub fn convert(&mut self, sdp: &str) -> String {
        let (session, media) = split_sections(sdp);
        let Some((first, rest)) = media.split_first() else {
            return sdp.to_owned();
        };
        if !first.first().is_some_and(|l| l.starts_with("m=audio")) {
            return sdp.to_owned();
        }
        if is_unified(&session) {
            return self.pass_unified(&session, &media);
        }

        // Without the mid header extension, libwebrtc routes incoming packets by ssrc.
        let template: Vec<&str> = first
            .iter()
            .copied()
            .filter(|l| !l.contains(MID_EXTENSION) && !l.starts_with("a=ssrc") && !l.starts_with("a=msid"))
            .collect();
        let main_mid = template
            .iter()
            .find_map(|l| l.strip_prefix("a=mid:"))
            .unwrap_or("0")
            .to_owned();
        let peers: Vec<RemoteStream> = parse_streams(first)
            .into_iter()
            .filter(|s| !s.is_own() && Some(s.ssrc) != self.own_ssrc)
            .collect();
        for peer in &peers {
            if !self.mids.iter().any(|(ssrc, _)| *ssrc == peer.ssrc) {
                let mid = format!("{main_mid}-{}", self.mids.len() + 1);
                self.mids.push((peer.ssrc, mid));
            }
        }

        let mut out: Vec<String> = Vec::new();
        let bundle = std::iter::once(main_mid.as_str())
            .chain(self.mids.iter().map(|(_, mid)| mid.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        for line in &session {
            if line.starts_with("a=group:BUNDLE") {
                out.push(format!("a=group:BUNDLE {bundle}"));
            } else {
                out.push((*line).to_owned());
            }
        }
        out.extend(template.iter().map(|l| (*l).to_owned()));
        // "-" is no stream: without any msid, libwebrtc puts the main section's track in
        // a default stream that it announces once and removes whenever msid reappears.
        out.push("a=msid:-".to_owned());
        for (ssrc, mid) in &self.mids {
            let peer = peers.iter().find(|p| p.ssrc == *ssrc);
            out.extend(
                template
                    .iter()
                    .filter(|l| !l.starts_with("a=mid") && !is_direction(l))
                    .map(|l| (*l).to_owned()),
            );
            out.push(format!("a=mid:{mid}"));
            match peer {
                Some(peer) => {
                    out.push("a=sendonly".to_owned());
                    out.extend(peer.lines());
                }
                None => out.push("a=inactive".to_owned()),
            }
        }
        for section in rest {
            out.extend(section.iter().map(|l| (*l).to_owned()));
        }
        let mut sdp = out.join("\r\n");
        sdp.push_str("\r\n");
        sdp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "v=0\r\n\
o=- 1 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0\r\n\
a=ice-lite\r\n\
m=audio 27015 UDP/TLS/RTP/SAVPF 111\r\n\
c=IN IP4 203.0.113.7\r\n\
a=mid:0\r\n\
a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\n\
a=sendrecv\r\n\
a=ssrc:1111 cname:self\r\n\
a=ssrc:1111 msid:Self-1-42-a self-track\r\n\
a=ssrc:2222 cname:peer-a\r\n\
a=ssrc:2222 msid:Peer-1-1001-a track-a\r\n\
a=ssrc:3333 cname:peer-b\r\n\
a=ssrc:3333 msid:Peer-2-1002-b track-b\r\n";

    fn sections(sdp: &str) -> Vec<Vec<String>> {
        split_sections(sdp)
            .1
            .into_iter()
            .map(|s| s.into_iter().map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn splits_peers_into_their_own_sections() {
        let mut conv = PlanBToUnifiedPlan::new();
        let out = conv.convert(ANSWER);
        assert!(out.contains("a=group:BUNDLE 0 0-1 0-2\r\n"));
        assert!(!out.contains(MID_EXTENSION));
        assert!(
            !out.contains("Self-"),
            "our own echoed ssrc must not become a remote stream"
        );
        let secs = sections(&out);
        assert_eq!(secs.len(), 3);
        assert!(secs[0].contains(&"a=mid:0".to_owned()));
        assert!(secs[0].contains(&"a=sendrecv".to_owned()));
        assert!(secs[0].contains(&"a=msid:-".to_owned()));
        assert!(secs[1].contains(&"a=mid:0-1".to_owned()));
        assert!(secs[1].contains(&"a=sendonly".to_owned()));
        assert!(secs[1].contains(&"a=ssrc:2222 msid:Peer-1-1001-a Peer-1-1001-a-audio".to_owned()));
        assert!(!secs[1].contains(&"a=sendrecv".to_owned()));
        assert_eq!(conv.ssrc_for_mid("0-2"), Some(3333));
        assert!(out.ends_with("\r\n"));
    }

    /// The shape Steam sent on 2026-11-03: already one m-section per participant.
    const UNIFIED_OFFER: &str = "v=0\r\n\
o=- 1 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0 m1\r\n\
a=ice-lite\r\n\
m=audio 27014 UDP/TLS/RTP/SAVPF 111\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
a=mid:0\r\n\
a=sendrecv\r\n\
a=rtcp-mux\r\n\
a=ssrc:1111 cname:Self-42-1111\r\n\
a=msid:Self-42-1111 1111\r\n\
a=label:Self-42-1111\r\n\
m=audio 27014 UDP/TLS/RTP/SAVPF 111\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
a=mid:m1\r\n\
a=sendrecv\r\n\
a=rtcp-mux\r\n\
a=ssrc:2222 cname:Peer-42-1001-2222\r\n\
a=msid:Peer-42-1001-2222 2222\r\n\
a=label:Peer-42-1001-2222\r\n\
a=bundle-only\r\n";

    #[test]
    fn passes_unified_plan_through_without_our_own_stream() {
        let mut conv = PlanBToUnifiedPlan::new();
        conv.set_own_ssrc(1111);
        let out = conv.convert(UNIFIED_OFFER);
        assert!(out.contains("a=group:BUNDLE 0 m1\r\n"));
        assert!(!out.contains("1111"), "our own stream must be gone: {out}");
        assert!(!out.contains("Self-"));
        let secs = sections(&out);
        assert_eq!(secs.len(), 2);
        assert!(secs[0].iter().any(|l| l.contains(MID_EXTENSION)));
        assert!(secs[0].contains(&"a=msid:-".to_owned()));
        assert!(secs[1].contains(&"a=mid:m1".to_owned()));
        assert!(secs[1].contains(&"a=ssrc:2222 cname:Peer-42-1001-2222".to_owned()));
        assert!(secs[1].contains(&"a=bundle-only".to_owned()));
        assert_eq!(conv.ssrc_for_mid("m1"), Some(2222));
    }

    #[test]
    fn drops_our_own_ssrc_without_a_self_label() {
        // Steam echoed our sending ssrc with no msid at all (2026-11-03 log).
        let sdp = ANSWER.replace("a=ssrc:1111 msid:Self-1-42-a self-track\r\n", "");
        let mut conv = PlanBToUnifiedPlan::new();
        conv.set_own_ssrc(1111);
        let out = conv.convert(&sdp);
        assert!(!out.contains("a=ssrc:1111"));
        assert!(out.contains("a=group:BUNDLE 0 0-1 0-2\r\n"));
        assert_eq!(conv.ssrc_for_mid("0-1"), Some(2222));
    }

    #[test]
    fn keeps_mids_and_marks_left_peers_inactive() {
        let mut conv = PlanBToUnifiedPlan::new();
        conv.convert(ANSWER);
        let without_a: String = ANSWER
            .lines()
            .filter(|l| !l.contains("2222"))
            .map(|l| format!("{l}\n"))
            .collect();
        let secs = sections(&conv.convert(&without_a));
        assert_eq!(secs.len(), 3);
        assert!(secs[1].contains(&"a=mid:0-1".to_owned()));
        assert!(secs[1].contains(&"a=inactive".to_owned()));
        assert!(!secs[1].iter().any(|l| l.starts_with("a=ssrc")));
        assert!(secs[2].contains(&"a=sendonly".to_owned()));
    }

    #[test]
    fn leaves_non_audio_descriptions_alone() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n";
        assert_eq!(PlanBToUnifiedPlan::new().convert(sdp), sdp);
    }

    #[test]
    fn munges_only_opus_fmtp() {
        let sdp = "a=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\na=rtpmap:96 VP8/90000\r\na=fmtp:96 x=1\r\n";
        let out = munge_opus_fmtp(sdp);
        assert!(out.contains("a=fmtp:111 minptime=10;useinbandfec=1;usedtx=1\r\n"));
        assert!(out.contains("a=fmtp:96 x=1\r\n"));
    }

    #[test]
    fn reads_account_ids_from_stream_ids() {
        assert_eq!(account_id_from_stream_id("Peer-3-123456-abc"), Some(123_456));
        assert_eq!(account_id_from_stream_id("Self-1-42-a"), None);
        assert_eq!(account_id_from_stream_id("Peer-x-1-a"), None);
    }

    #[test]
    fn json_round_trip() {
        let json = to_json("offer", "v=0\r\n");
        assert_eq!(from_json(&json), Some(("offer".to_owned(), "v=0\r\n".to_owned())));
    }
}
