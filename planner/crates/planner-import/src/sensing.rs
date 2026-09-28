//! Sensing-node log: what a passive RX-only receiver writes down.
//!
//! WHY A SESSION HEADER. Every previous data source in this project arrived
//! without the two fields that decide the answer — the receiver's antenna
//! height and its gain. MeshCore adverts carry neither; the public Meshtastic
//! feed carries neither; and re-deriving them months later from a photograph
//! of a mast is how survey data dies. So a log FILE is self-describing: it
//! opens with a session record naming where the receiver was, how high, on
//! what antenna, and a log with no session record is rejected rather than
//! silently treated as if the metadata were zero.
//!
//! WHY NDJSON. One record per line, append-only, and a truncated final line
//! (power loss mid-write, a serial link pulled) costs one packet instead of
//! the file. It streams, it is greppable in the field, and the node can write
//! it with no allocator. The parser is deliberately tolerant of unknown
//! fields so a firmware that learns to report more does not break an older
//! host.
//!
//! WHY THE SAME FORMAT FOR EVERY TRANSPORT. The node may hand its log over
//! Wi-Fi, over USB serial, or on an SD card. Those are three ways of moving
//! the same bytes; making them three formats would mean three parsers and
//! three sets of bugs. The transport is the caller's problem, this module
//! only ever sees a `Read`.
//!
//! Format (NDJSON, one object per line):
//! ```text
//! {"kind":"session","id":"tegel-roof","lat":52.5869,"lon":13.2800,
//!  "h_agl_m":18.5,"ant_gain_dbi":2.15,"ant":"half-wave whip","hw":"lr2021",
//!  "fw":"planner-sense 0.1","t":1788400000,"note":"north parapet"}
//! {"kind":"rx","t":1788400012,"rssi":-112.5,"snr":-7.25,
//!  "sf":8,"bw":62.5,"cr":8,"f":869.618,
//!  "src":"01000001536e...","type":"advert","lat":52.5344,"lon":13.4038}
//! ```

use crate::{Observation, PositionQuality, SourceTag};
use planner_core::geo::GeoPos;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SensingError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {msg}")]
    Record { line: usize, msg: String },
    #[error(
        "no session record: a sensing log must open with a {{\"kind\":\"session\"}} line naming \
         the receiver's position, antenna height and gain. Without them the measurements cannot \
         be turned into path loss."
    )]
    NoSession,
}

/// Where the receiver was and what it was listening with.
///
/// Recorded ONCE per log. Every field here is one the packets themselves
/// cannot supply, which is exactly why they are mandatory in the file rather
/// than optional arguments to whoever processes it later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Stable site name, e.g. "tegel-roof". Used to join sessions together.
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    /// Antenna height above GROUND, metres. Surveyed, not guessed.
    pub h_agl_m: f64,
    /// Antenna gain (dBi). Needed to recover path loss from RSSI, and the
    /// single largest controllable error in that conversion.
    pub ant_gain_dbi: f64,
    /// Free text: antenna model, so a gain figure can be checked later.
    #[serde(default)]
    pub ant: String,
    /// Receiver hardware, e.g. "lr2021" or "sx1262". Absolute RSSI accuracy is
    /// unspecified on BOTH parts, so a calibration is only transferable to the
    /// same silicon; recording which chip measured it is what makes that
    /// checkable instead of assumed.
    #[serde(default)]
    pub hw: String,
    #[serde(default)]
    pub fw: String,
    /// Session start, Unix seconds.
    #[serde(default)]
    pub t: Option<i64>,
    #[serde(default)]
    pub note: String,
}

/// One received packet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rx {
    /// Unix seconds. `None` when the node has no clock — the record is still
    /// usable for path loss, just not for time-of-day analysis, so it is not
    /// rejected.
    #[serde(default)]
    pub t: Option<i64>,
    pub rssi: f32,
    #[serde(default)]
    pub snr: Option<f32>,
    #[serde(default)]
    pub sf: Option<u8>,
    #[serde(default)]
    pub bw: Option<f64>,
    #[serde(default)]
    pub cr: Option<u8>,
    #[serde(default)]
    pub f: Option<f64>,
    /// Sender identity as the radio reported it (MeshCore public key or a
    /// prefix of it). Empty when the packet is not attributable.
    #[serde(default)]
    pub src: String,
    /// Packet type as the node classified it, e.g. "advert".
    #[serde(default, rename = "type")]
    pub kind_of: String,
    /// Sender's SELF-REPORTED position, when the packet carried one.
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
    /// MeshCore route type as the node read it off the header byte: "flood",
    /// "transport-flood", "direct" or "transport-direct". `None` for a log
    /// written before the node recorded it, or for a frame that did not decode.
    #[serde(default)]
    pub route: Option<String>,
    /// Hops the packet HAD ALREADY TRAVELLED when this receiver heard it.
    ///
    /// Only ever set for flood routing, where a relay APPENDS its own hash
    /// before rebroadcasting (mcrs `prepare_flood_forward` -> `append_flood_hop`)
    /// so the path is the route already taken.
    ///
    /// This is the field that decides whether an RSSI can become a path-loss
    /// observation, and it is the reason it exists. MeshCore floods adverts up
    /// to `DEFAULT_FLOOD_MAX_ADVERT_HOPS = 3`, so in a dense mesh MOST heard
    /// adverts have been relayed -- and the RSSI then belongs to the last
    /// relay's transmission, not to the advertiser whose position the advert
    /// carries. Attributing it to the advertiser reports a link that was never
    /// measured.
    #[serde(default)]
    pub hops_travelled: Option<u8>,
    /// Hops the packet still had to make, for DIRECT routing.
    ///
    /// A separate field from `hops_travelled` and never merged with it: under
    /// direct routing a relay REMOVES the first hash as the packet advances
    /// (`consume_direct_hop` -> `Path::remove_first_hash`), so the path names
    /// nodes that have NOT carried the packet and the count is hops remaining.
    /// Two different quantities must not share a key name, or a hop-count
    /// histogram silently pools them.
    #[serde(default)]
    pub hops_remaining: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SensingLog {
    pub session: Session,
    pub rx: Vec<Rx>,
    /// Lines that did not parse, reported rather than dropped. A field log
    /// that silently discards a tenth of its packets looks exactly like a
    /// quiet night on the air.
    pub bad_lines: Vec<(usize, String)>,
    /// Records skipped because they carried no usable RSSI.
    pub no_rssi: usize,
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum Record {
    #[serde(rename = "session")]
    Session(Session),
    #[serde(rename = "rx")]
    Rx(Rx),
}

/// Parse a sensing log.
///
/// A later session record REPLACES the current one: a node restarted after
/// being moved up a mast writes a new header, and the packets after it belong
/// to the new geometry. Splitting on that is the caller's job via
/// [`parse_sessions`]; this function returns the FIRST session and all packets,
/// which is right for the common single-session file and wrong for a merged
/// one — so it errors if the session changes.
pub fn parse<R: Read>(reader: R) -> Result<SensingLog, SensingError> {
    let mut sessions = parse_sessions(reader)?;
    if sessions.len() > 1 {
        return Err(SensingError::Record {
            line: 0,
            msg: format!(
                "{} session records in one log — the receiver moved or changed antenna \
                 mid-file; use parse_sessions() so each geometry keeps its own packets",
                sessions.len()
            ),
        });
    }
    sessions.pop().ok_or(SensingError::NoSession)
}

/// Parse a log that may contain several sessions, in order.
pub fn parse_sessions<R: Read>(reader: R) -> Result<Vec<SensingLog>, SensingError> {
    let mut out: Vec<SensingLog> = Vec::new();
    for (i, line) in BufReader::new(reader).lines().enumerate() {
        let lineno = i + 1;
        let line = line?;
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        match serde_json::from_str::<Record>(s) {
            Ok(Record::Session(sess)) => {
                if !sess.h_agl_m.is_finite() || !sess.ant_gain_dbi.is_finite() {
                    return Err(SensingError::Record {
                        line: lineno,
                        msg: "session height and antenna gain must be finite numbers".into(),
                    });
                }
                out.push(SensingLog {
                    session: sess,
                    rx: Vec::new(),
                    bad_lines: Vec::new(),
                    no_rssi: 0,
                });
            }
            Ok(Record::Rx(r)) => {
                let Some(cur) = out.last_mut() else {
                    return Err(SensingError::NoSession);
                };
                // A non-finite RSSI is not a weak signal, it is a broken
                // reading; counting it as -inf dBm would drag any fit.
                if !r.rssi.is_finite() {
                    cur.no_rssi += 1;
                    continue;
                }
                cur.rx.push(r);
            }
            Err(e) => match out.last_mut() {
                Some(cur) => cur.bad_lines.push((lineno, e.to_string())),
                None => {
                    return Err(SensingError::Record { line: lineno, msg: e.to_string() })
                }
            },
        }
    }
    Ok(out)
}

impl Rx {
    /// Whether this packet reached the receiver DIRECTLY from the node its
    /// `src` names, so that its RSSI measures that one link.
    ///
    /// Three answers, and the third is the point:
    ///   * `Some(true)`  -- flood routing, zero hops travelled. The carrier we
    ///     measured came off the transmitter `src` names.
    ///   * `Some(false)` -- it was relayed, or it was direct-routed (in which
    ///     case the immediate transmitter is not named in the packet at all).
    ///   * `None` -- the log does not say. A file written before the node
    ///     recorded hop counts cannot distinguish the two, and treating an
    ///     unknown as zero would silently re-admit every relayed advert the
    ///     gate exists to exclude.
    pub fn measures_one_link(&self) -> Option<bool> {
        if self.hops_remaining.is_some() {
            // Direct routing: the path is the route ahead and the node that
            // actually transmitted this frame is not in the record.
            return Some(false);
        }
        self.hops_travelled.map(|h| h == 0)
    }
}

impl SensingLog {
    /// Turn the log into [`Observation`]s.
    ///
    /// Three gates, and the third is new. A packet becomes an observation only
    /// when it carries a sender identity, a sender position, AND arrived over
    /// ONE measured link.
    ///
    /// WHY THE HOP GATE. An advert names the advertiser and carries the
    /// advertiser's position, so it looks like a ready-made path-loss sample.
    /// But MeshCore floods adverts up to three hops
    /// (`DEFAULT_FLOOD_MAX_ADVERT_HOPS = 3`, mcrs `firmware/src/app/config.rs`)
    /// and a repeater appends its own hash and rebroadcasts. The RSSI recorded
    /// for a relayed advert is therefore the RELAY's transmission, measured
    /// over a link between two nodes one of which the record does not name --
    /// while the position in it belongs to the original advertiser, possibly
    /// kilometres away. Feeding that pair into a calibration reports "repeater
    /// X is reachable from this site at -112 dBm" for a link nobody ever
    /// measured. In a dense mesh that is the MAJORITY of heard adverts.
    ///
    /// WHY AN UNKNOWN HOP COUNT IS ALSO REFUSED. Logs written before the node
    /// recorded hop counts contain exactly the same mixture and no way to
    /// separate it. Admitting them because the field is missing would keep the
    /// defect alive under a new name. They are counted in their own bucket
    /// (`hops_unknown`) and reported, so the loss is visible rather than
    /// silent -- and re-collecting from the node recovers them, because the
    /// records are still in the ring.
    pub fn observations(&self) -> (Vec<Observation>, SensingYield) {
        let rx_pos = GeoPos { lat_deg: self.session.lat, lon_deg: self.session.lon };
        let mut out = Vec::new();
        let mut y = SensingYield { total: self.rx.len(), ..Default::default() };
        for r in &self.rx {
            // Checked HERE and not only in the parser: records also arrive from
            // transport shims that construct `Rx` directly, and this is the gate
            // every calibration set passes through.
            if !r.rssi.is_finite() {
                y.bad_rssi += 1;
                continue;
            }
            if r.src.is_empty() {
                y.no_sender += 1;
                continue;
            }
            let (Some(lat), Some(lon)) = (r.lat, r.lon) else {
                y.no_tx_position += 1;
                continue;
            };
            if !lat.is_finite() || !lon.is_finite() {
                y.no_tx_position += 1;
                continue;
            }
            match r.measures_one_link() {
                Some(true) => {}
                Some(false) => {
                    y.relayed += 1;
                    continue;
                }
                None => {
                    y.hops_unknown += 1;
                    continue;
                }
            }
            y.usable += 1;
            out.push(Observation {
                time_unix: r.t,
                tx_id: r.src.clone(),
                rx_id: self.session.id.clone(),
                tx_pos: Some(GeoPos { lat_deg: lat, lon_deg: lon }),
                rx_pos: Some(rx_pos),
                // The RECEIVER is a surveyed fixed site; the TRANSMITTER's
                // position is self-reported in its advert. The weaker of the
                // two is what the pair is worth, so the pair is flagged as
                // the advert's quality, not the survey's.
                pos_quality: PositionQuality::FixedSite,
                snr_db: r.snr,
                rssi_dbm: Some(r.rssi),
                freq_mhz: r.f,
                source: SourceTag::MeshCoreCli,
            });
        }
        (out, y)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SensingYield {
    pub total: usize,
    pub usable: usize,
    pub no_sender: usize,
    pub no_tx_position: usize,
    /// Records whose RSSI was not a finite number. Counted, never silently
    /// dropped: a log that quietly discards readings looks like a quiet night.
    pub bad_rssi: usize,
    /// Packets that named a sender and a position but did NOT arrive over one
    /// measured link: relayed under flood, or direct-routed so that the
    /// immediate transmitter is not named at all. Their RSSI measures a link
    /// the record cannot identify. Counted rather than dropped, because in a
    /// dense mesh this is the largest single bucket and a survey that does not
    /// see it concludes it heard far less than it did.
    pub relayed: usize,
    /// Packets from a log that predates hop-count recording. Neither admitted
    /// nor forgotten: re-collecting from the node recovers them.
    pub hops_unknown: usize,
}

impl std::fmt::Display for SensingYield {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} packet(s) -> {} observation(s) ({} unattributable, {} without a sender position, \
             {} with an unusable RSSI, {} relayed rather than heard directly, {} from a log that \
             does not record hop counts)",
            self.total,
            self.usable,
            self.no_sender,
            self.no_tx_position,
            self.bad_rssi,
            self.relayed,
            self.hops_unknown
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = r#"{"kind":"session","id":"tegel-roof","lat":52.5869,"lon":13.28,
        "h_agl_m":18.5,"ant_gain_dbi":2.15,"ant":"half-wave whip","hw":"lr2021"}"#;

    fn line(s: &str) -> String {
        s.replace('\n', " ")
    }

    #[test]
    fn a_log_without_a_session_record_is_rejected_not_assumed() {
        // The alternative is silently treating an unknown antenna height as
        // zero, which is the failure this whole project keeps hitting.
        let text = r#"{"kind":"rx","rssi":-110.0,"src":"aa","lat":52.5,"lon":13.4}"#;
        let e = parse(text.as_bytes()).unwrap_err();
        assert!(matches!(e, SensingError::NoSession), "got {e:?}");
    }

    #[test]
    fn a_truncated_final_line_costs_one_packet_not_the_file() {
        let text = format!(
            "{}\n{}\n{}",
            line(SESSION),
            r#"{"kind":"rx","rssi":-101.5,"snr":-3.0,"src":"aa","lat":52.5,"lon":13.4}"#,
            r#"{"kind":"rx","rssi":-104.0,"src":"bb","la"#,
        );
        let log = parse(text.as_bytes()).unwrap();
        assert_eq!(log.rx.len(), 1);
        assert_eq!(log.bad_lines.len(), 1, "the broken line must be reported");
        assert_eq!(log.bad_lines[0].0, 3);
    }

    #[test]
    fn unknown_fields_from_a_newer_firmware_do_not_break_an_older_host() {
        let text = format!(
            "{}\n{}",
            line(SESSION),
            r#"{"kind":"rx","rssi":-99.0,"src":"aa","lat":52.5,"lon":13.4,
                "future_field":42,"another":{"nested":true}}"#
                .replace('\n', " "),
        );
        let log = parse(text.as_bytes()).unwrap();
        assert_eq!(log.rx.len(), 1);
        assert!(log.bad_lines.is_empty());
    }

    #[test]
    fn an_out_of_range_rssi_never_becomes_a_measurement() {
        // JSON cannot express NaN or Infinity, so an overflowing literal is
        // what a broken reading actually looks like on the wire — and
        // serde_json rejects it outright, so it lands in `bad_lines` rather
        // than in the finite-check below. Either route is acceptable; what is
        // NOT acceptable is it surviving into `rx` as if it were a very weak
        // signal, which would drag any fit toward minus infinity.
        let text = format!(
            "{}\n{}",
            line(SESSION),
            r#"{"kind":"rx","rssi":-1e400,"src":"aa","lat":52.5,"lon":13.4}"#,
        );
        let log = parse(text.as_bytes()).unwrap();
        assert!(log.rx.is_empty(), "an unrepresentable RSSI must not be kept");
        assert_eq!(
            log.no_rssi + log.bad_lines.len(),
            1,
            "and it must be REPORTED by one route or the other, never dropped silently"
        );
        let (obs, y) = log.observations();
        assert!(obs.is_empty());
        assert_eq!(y.total, 0);
    }

    #[test]
    fn the_finite_guard_holds_for_records_built_in_code() {
        // The parser is not the only way records arrive: a transport shim (a
        // serial reader, a Wi-Fi puller) can construct `Rx` directly, and that
        // path has no serde to reject a non-finite float.
        let text = line(SESSION);
        let mut log = parse_sessions(text.as_bytes()).unwrap().pop().unwrap();
        log.rx.push(Rx {
            t: None, rssi: f32::NEG_INFINITY, snr: None, sf: None, bw: None,
            cr: None, f: None, src: "aa".into(), kind_of: String::new(),
            lat: Some(52.5), lon: Some(13.4),
            route: Some("flood".into()), hops_travelled: Some(0), hops_remaining: None,
        });
        // observations() is the gate that matters — a non-finite RSSI must not
        // reach a calibration set.
        let (obs, _) = log.observations();
        assert!(
            obs.iter().all(|o| o.rssi_dbm.is_none_or(|v| v.is_finite())),
            "a non-finite RSSI reached an Observation"
        );
    }

    #[test]
    fn only_packets_with_a_sender_and_a_position_become_observations() {
        let text = format!(
            "{}\n{}\n{}\n{}",
            line(SESSION),
            line(
                r#"{"kind":"rx","rssi":-101.0,"src":"aa","lat":52.5,"lon":13.4,"type":"advert",
                    "route":"flood","hops_travelled":0}"#
            ),
            line(r#"{"kind":"rx","rssi":-108.0,"src":"","lat":52.5,"lon":13.4}"#),
            line(r#"{"kind":"rx","rssi":-95.0,"src":"cc"}"#),
        );
        let log = parse(text.as_bytes()).unwrap();
        let (obs, y) = log.observations();
        assert_eq!(y.total, 3);
        assert_eq!(y.usable, 1);
        assert_eq!(y.no_sender, 1);
        assert_eq!(y.no_tx_position, 1);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].tx_id, "aa");
        assert_eq!(obs[0].rx_id, "tegel-roof");
        assert_eq!(obs[0].rssi_dbm, Some(-101.0));
        // The receiver position comes from the session, not the packet.
        assert_eq!(obs[0].rx_pos.unwrap().lat_deg, 52.5869);
    }

    #[test]
    fn a_moved_receiver_writes_a_new_session_and_keeps_its_packets_separate() {
        // Raising the antenna mid-log changes the geometry of every subsequent
        // path. Merging both halves under one header would silently attribute
        // the new heights to the old measurements.
        let second = r#"{"kind":"session","id":"tegel-roof","lat":52.5869,"lon":13.28,
            "h_agl_m":24.0,"ant_gain_dbi":2.15}"#;
        let text = format!(
            "{}\n{}\n{}\n{}",
            line(SESSION),
            r#"{"kind":"rx","rssi":-101.0,"src":"aa","lat":52.5,"lon":13.4}"#,
            line(second),
            r#"{"kind":"rx","rssi":-97.0,"src":"aa","lat":52.5,"lon":13.4}"#,
        );
        let sessions = parse_sessions(text.as_bytes()).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session.h_agl_m, 18.5);
        assert_eq!(sessions[1].session.h_agl_m, 24.0);
        assert_eq!(sessions[0].rx.len(), 1);
        assert_eq!(sessions[1].rx.len(), 1);
        // And the single-session helper refuses rather than merging them.
        assert!(parse(text.as_bytes()).is_err());
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let text = format!("# survey 2026-09-01\n\n{}\n\n{}\n", line(SESSION),
            r#"{"kind":"rx","rssi":-100.0,"src":"aa","lat":52.5,"lon":13.4}"#);
        let log = parse(text.as_bytes()).unwrap();
        assert_eq!(log.rx.len(), 1);
        assert!(log.bad_lines.is_empty());
    }

    #[test]
    fn an_advert_that_arrived_by_three_hops_is_not_counted_as_a_path_loss_observation() {
        // The RSSI belongs to whichever repeater rebroadcast it; the position
        // belongs to the advertiser, three hops away. Pairing them reports a
        // link that was never measured. MeshCore floods adverts up to three
        // hops by default, so this is the common case rather than the exotic
        // one.
        let text = format!(
            "{}\n{}\n{}",
            line(SESSION),
            line(
                r#"{"kind":"rx","rssi":-112.0,"src":"aa","lat":52.5,"lon":13.4,"type":"advert",
                    "route":"flood","hops_travelled":3}"#
            ),
            line(
                r#"{"kind":"rx","rssi":-101.0,"src":"bb","lat":52.6,"lon":13.3,"type":"advert",
                    "route":"flood","hops_travelled":0}"#
            ),
        );
        let log = parse(text.as_bytes()).unwrap();
        let (obs, y) = log.observations();
        assert_eq!(y.total, 2);
        assert_eq!(y.usable, 1, "only the zero-hop advert measures one link");
        assert_eq!(y.relayed, 1);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].tx_id, "bb");
    }

    #[test]
    fn a_direct_routed_packet_is_never_a_path_loss_observation_at_any_hop_count() {
        // Under direct routing the path is the route AHEAD, so even
        // "hops_remaining":0 does not mean the sender transmitted the frame we
        // measured -- and the immediate transmitter is not named at all.
        let text = format!(
            "{}\n{}",
            line(SESSION),
            line(
                r#"{"kind":"rx","rssi":-99.0,"src":"aa","lat":52.5,"lon":13.4,
                    "route":"direct","hops_remaining":0}"#
            ),
        );
        let log = parse(text.as_bytes()).unwrap();
        let (obs, y) = log.observations();
        assert!(obs.is_empty());
        assert_eq!(y.relayed, 1);
    }

    #[test]
    fn a_log_that_predates_hop_counts_is_reported_as_unknown_rather_than_admitted() {
        // Treating a missing hop count as zero would silently re-admit every
        // relayed advert the gate exists to exclude.
        let text = format!(
            "{}\n{}",
            line(SESSION),
            r#"{"kind":"rx","rssi":-101.0,"src":"aa","lat":52.5,"lon":13.4,"type":"advert"}"#,
        );
        let log = parse(text.as_bytes()).unwrap();
        let (obs, y) = log.observations();
        assert!(obs.is_empty());
        assert_eq!(y.hops_unknown, 1);
        assert_eq!(y.usable, 0);
        // And it is REPORTED, not merely absent from the count.
        assert!(y.to_string().contains("does not record hop counts"));
    }

    #[test]
    fn hops_travelled_and_hops_remaining_never_share_a_key() {
        // Two different quantities under one name is how a hop-count
        // distribution silently becomes two distributions in one histogram.
        let flood = Rx {
            t: None, rssi: -100.0, snr: None, sf: None, bw: None, cr: None, f: None,
            src: "aa".into(), kind_of: String::new(), lat: None, lon: None,
            route: Some("flood".into()), hops_travelled: Some(2), hops_remaining: None,
        };
        let direct = Rx { route: Some("direct".into()), hops_travelled: None,
            hops_remaining: Some(2), ..flood.clone() };
        assert_eq!(flood.measures_one_link(), Some(false));
        assert_eq!(direct.measures_one_link(), Some(false));
        let json = serde_json::to_string(&direct).unwrap();
        assert!(json.contains("hops_remaining"));
        assert!(!json.contains("\"hops_travelled\":2"));
    }

    #[test]
    fn a_session_with_a_non_finite_height_is_an_error_not_a_default() {
        let bad = r#"{"kind":"session","id":"x","lat":52.0,"lon":13.0,
            "h_agl_m":null,"ant_gain_dbi":2.0}"#;
        assert!(parse(line(bad).as_bytes()).is_err());
    }
}
