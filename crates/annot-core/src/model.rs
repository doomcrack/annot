//! Record/anchor data model and the JSON schema serde mapping.

pub use ulid::Ulid;

/// Current UTC time as RFC 3339 with millisecond precision, e.g.
/// `2026-08-02T18:03:11.123Z`.
pub fn now_rfc3339() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    rfc3339_from_millis(millis)
}

fn rfc3339_from_millis(millis: u64) -> String {
    let secs = (millis / 1000) as i64;
    let subsec_millis = millis % 1000;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{subsec_millis:03}Z")
}

/// Days-since-epoch (1970-01-01) to proleptic Gregorian (year, month, day).
/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// A single annotation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub id: Ulid,
    pub kind: Kind,
    pub body: String,
    pub anchor: Anchor,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_snippet: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Record {
    /// New live record: fresh id, `orig_snippet = None`, `created_at == updated_at == now`.
    pub fn new(kind: Kind, body: String, anchor: Anchor) -> Self {
        let now = now_rfc3339();
        Self {
            id: Ulid::generate(),
            kind,
            body,
            anchor,
            status: Status::Live,
            orig_snippet: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Sets `updated_at` to now.
    pub fn touch(&mut self) {
        self.updated_at = now_rfc3339();
    }

    /// Marks the record orphaned and touches `updated_at`.
    pub fn orphan(&mut self) {
        self.status = Status::Orphaned;
        self.touch();
    }

    /// Marks the record tombstoned and touches `updated_at`.
    pub fn mark_tombstone(&mut self) {
        self.status = Status::Tombstone;
        self.touch();
    }
}

/// The anchored code location an annotation is attached to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    pub base_blob: String,
    pub start: u32,
    pub end: u32,
    pub line_hashes: Vec<String>,
    pub ctx_before: String,
    pub ctx_after: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

impl Anchor {
    /// Number of anchored lines: `end - start + 1`.
    pub fn line_count(&self) -> u32 {
        self.end - self.start + 1
    }
}

/// Annotation category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Decision,
    Gotcha,
    Todo,
    History,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Decision, Kind::Gotcha, Kind::Todo, Kind::History];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Decision => "decision",
            Kind::Gotcha => "gotcha",
            Kind::Todo => "todo",
            Kind::History => "history",
        }
    }
}

impl std::str::FromStr for Kind {
    type Err = ParseKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "decision" => Ok(Kind::Decision),
            "gotcha" => Ok(Kind::Gotcha),
            "todo" => Ok(Kind::Todo),
            "history" => Ok(Kind::History),
            other => Err(ParseKindError(other.to_string())),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Annotation lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Live,
    Orphaned,
    Tombstone,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Live => "live",
            Status::Orphaned => "orphaned",
            Status::Tombstone => "tombstone",
        }
    }
}

impl std::str::FromStr for Status {
    type Err = ParseStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "live" => Ok(Status::Live),
            "orphaned" => Ok(Status::Orphaned),
            "tombstone" => Ok(Status::Tombstone),
            other => Err(ParseStatusError(other.to_string())),
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown kind {0:?} (expected decision|gotcha|todo|history)")]
pub struct ParseKindError(pub String);

#[derive(Debug, thiserror::Error)]
#[error("unknown status {0:?} (expected live|orphaned|tombstone)")]
pub struct ParseStatusError(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_anchor() -> Anchor {
        Anchor {
            base_blob: "a".repeat(40),
            start: 141,
            end: 158,
            line_hashes: vec!["0123456789abcdef".to_string(); 18],
            ctx_before: "fedcba9876543210".to_string(),
            ctx_after: "1111111111111111".to_string(),
            symbol: Some("fn parse_expr".to_string()),
        }
    }

    fn spec_example_json() -> serde_json::Value {
        serde_json::json!({
            "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "kind": "decision",
            "body": "chose X because Y",
            "anchor": {
                "base_blob": "a".repeat(40),
                "start": 141,
                "end": 158,
                "line_hashes": vec!["0123456789abcdef".to_string(); 18],
                "ctx_before": "fedcba9876543210",
                "ctx_after": "1111111111111111",
                "symbol": "fn parse_expr"
            },
            "status": "live",
            "orig_snippet": "let x = 1;",
            "created_at": "2026-08-02T18:03:11.123Z",
            "updated_at": "2026-08-02T18:03:11.123Z"
        })
    }

    #[test]
    fn record_roundtrip_spec_example() {
        let value = spec_example_json();
        let record: Record = serde_json::from_value(value.clone()).unwrap();
        let reserialized = serde_json::to_value(&record).unwrap();
        assert_eq!(reserialized, value);
    }

    #[test]
    fn optional_keys_absent_when_none() {
        let mut anchor = sample_anchor();
        anchor.symbol = None;
        let record = Record::new(Kind::Gotcha, "body".to_string(), anchor);
        let value = serde_json::to_value(&record).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("orig_snippet"));
        assert!(!obj["anchor"].as_object().unwrap().contains_key("symbol"));
    }

    #[test]
    fn kind_status_serde_and_fromstr() {
        for kind in Kind::ALL {
            let s = kind.to_string();
            assert_eq!(s.parse::<Kind>().unwrap(), kind);
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{s}\""));
            let back: Kind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
        assert!("bogus".parse::<Kind>().is_err());

        for status in [Status::Live, Status::Orphaned, Status::Tombstone] {
            let s = status.to_string();
            assert_eq!(s.parse::<Status>().unwrap(), status);
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{s}\""));
        }
        assert!("bogus".parse::<Status>().is_err());
    }

    #[test]
    fn unknown_json_field_tolerated() {
        let mut value = spec_example_json();
        value
            .as_object_mut()
            .unwrap()
            .insert("future_field".to_string(), serde_json::json!("ignored"));
        let record: Result<Record, _> = serde_json::from_value(value);
        assert!(record.is_ok());
    }

    #[test]
    fn rfc3339_from_millis_known_epochs() {
        assert_eq!(rfc3339_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_from_millis(7), "1970-01-01T00:00:00.007Z");
        assert_eq!(
            rfc3339_from_millis(1_709_164_800_000),
            "2024-02-29T00:00:00.000Z"
        );
        assert_eq!(
            rfc3339_from_millis(1_785_693_791_123),
            "2026-08-02T18:03:11.123Z"
        );
    }

    #[test]
    fn record_new_sets_live_ulid_and_timestamps() {
        let before = now_rfc3339();
        let record = Record::new(Kind::Todo, "todo body".to_string(), sample_anchor());
        let after = now_rfc3339();
        assert_eq!(record.status, Status::Live);
        assert!(record.orig_snippet.is_none());
        assert_eq!(record.created_at, record.updated_at);
        assert!(record.created_at.as_str() >= before.as_str());
        assert!(record.created_at.as_str() <= after.as_str());
    }

    #[test]
    fn mark_tombstone_and_orphan_touch_updated_at() {
        let mut record = Record::new(Kind::History, "h".to_string(), sample_anchor());
        let created = record.created_at.clone();

        record.orphan();
        assert_eq!(record.status, Status::Orphaned);
        assert!(record.updated_at.as_str() >= created.as_str());

        record.mark_tombstone();
        assert_eq!(record.status, Status::Tombstone);
    }

    #[test]
    fn anchor_line_count() {
        assert_eq!(sample_anchor().line_count(), 18);
    }
}
