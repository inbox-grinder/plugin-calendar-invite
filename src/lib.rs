//! Conservative single-event extraction. Unsupported calendars remain ordinary mail.
use chrono::{DateTime, NaiveDateTime, SecondsFormat};
use plugin_sdk::{
    EmailInput, FieldCandidate, RecordContribution, RecordIdentity, RecordResult, SemanticUpdate,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
plugin_sdk::export_plugin!(extract);

fn hash(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn timestamp(s: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.to_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
        .or_else(|| {
            NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ")
                .ok()
                .map(|d| d.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
        })
}
fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(match chars.next()? {
                'n' | 'N' => '\n',
                '\\' => '\\',
                ',' => ',',
                ';' => ';',
                _ => return None,
            });
        } else {
            out.push(c);
        }
    }
    Some(out)
}
#[derive(Debug)]
struct Event {
    uid: String,
    title: String,
    start: String,
    end: String,
    location: String,
}
fn event(fields: BTreeMap<String, String>) -> Option<Event> {
    let get = |name: &str| fields.get(name).filter(|v| !v.trim().is_empty()).cloned();
    let start = timestamp(&get("DTSTART")?)?;
    let end = timestamp(&get("DTEND")?)?;
    if start >= end {
        return None;
    }
    Some(Event {
        uid: get("UID")?,
        title: get("SUMMARY")?,
        start,
        end,
        location: get("LOCATION")?,
    })
}
fn calendar(text: &str) -> Option<Event> {
    if text.len() > 64 * 1024 {
        return None;
    }
    let mut lines: Vec<String> = vec![];
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            lines.last_mut()?.push_str(&line[1..]);
        } else {
            lines.push(line.to_owned());
        }
    }
    if lines.first()?.as_str() != "BEGIN:VCALENDAR" || lines.last()?.as_str() != "END:VCALENDAR" {
        return None;
    }
    let mut fields = BTreeMap::new();
    let mut inside = false;
    let mut count = 0;
    for line in &lines[1..lines.len() - 1] {
        let (name, value) = line.split_once(':')?;
        let name = name.to_ascii_uppercase();
        let name = name.as_str();
        if name == "METHOD"
            && !["REQUEST", "PUBLISH"]
                .iter()
                .any(|m| value.eq_ignore_ascii_case(m))
        {
            return None;
        }
        match (name, value) {
            ("BEGIN", "VEVENT") if !inside => {
                inside = true;
                count += 1;
            }
            ("END", "VEVENT") if inside => inside = false,
            ("BEGIN" | "END", _) => return None,
            _ if inside => {
                let key = name.split(';').next()?;
                if ["RRULE", "RDATE", "EXDATE", "RECURRENCE-ID", "STATUS"].contains(&key) {
                    return None;
                }
                if ["UID", "SUMMARY", "DTSTART", "DTEND", "LOCATION"].contains(&key) {
                    // Floating times, named zones and all-day values are deliberately unsupported.
                    if key.starts_with("DT") && name != key {
                        return None;
                    }
                    if fields.insert(key.into(), unescape(value)?).is_some() {
                        return None;
                    }
                }
            }
            _ => {}
        }
    }
    if inside || count != 1 {
        return None;
    }
    event(fields)
}
fn text_event(text: &str) -> Option<Event> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let name = match key.trim().to_ascii_lowercase().as_str() {
            "event id" => "UID",
            "event" | "title" => "SUMMARY",
            "start" => "DTSTART",
            "end" => "DTEND",
            "location" => "LOCATION",
            _ => continue,
        };
        if fields.insert(name.into(), value.trim().into()).is_some() {
            return None;
        }
    }
    event(fields)
}
pub fn extract(input: EmailInput) -> RecordResult {
    let mut result = RecordResult {
        schema_version: "1".into(),
        sources: input
            .invocation
            .as_ref()
            .map(|i| i.sources.clone())
            .unwrap_or_default(),
        records: vec![],
        refresh_requests: vec![],
    };
    let Some(source) = input.message.source_id.as_ref().filter(|id| {
        result
            .sources
            .iter()
            .any(|s| &s.id == *id && s.kind == "email")
    }) else {
        return result;
    };
    let Some(observed) = input
        .date
        .as_deref()
        .and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_rfc2822(s))
                .ok()
        })
        .map(|d| d.to_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
    else {
        return result;
    };
    let parsed = if input.calendar_attachments.is_empty() {
        text_event(input.text_body.as_deref().unwrap_or(""))
    } else if input.calendar_attachments.len() == 1
        && input.calendar_attachments[0].content_type == "text/calendar"
    {
        calendar(&input.calendar_attachments[0].content)
    } else {
        None
    };
    let Some(e) = parsed else {
        return result;
    };
    if e.uid.len() > 512 || e.title.len() > 512 || e.location.len() > 2048 {
        return result;
    }
    let identity = RecordIdentity {
        namespace: "calendar-invite".into(),
        key: hash(&json!(e.uid)),
    };
    let update_key = format!(
        "event:{}",
        hash(&json!([e.title, e.start, e.end, e.location]))
    );
    let occurred = input
        .invocation
        .as_ref()
        .and_then(|i| i.records.iter().find(|r| r["identity"] == json!(identity)))
        .and_then(|r| r["updates"][&update_key]["occurredAt"].as_str())
        .unwrap_or(&observed)
        .to_owned();
    let fields: BTreeMap<String, _> = [
        ("eventTitle", json!(e.title)),
        ("startAt", json!(e.start)),
        ("endAt", json!(e.end)),
        ("location", json!(e.location)),
    ]
    .into_iter()
    .map(|(name, value)| {
        (
            name.into(),
            FieldCandidate {
                value,
                evidence: vec![source.clone()],
                basis: "extracted".into(),
                confidence: 1.0,
                observed_at: occurred.clone(),
                revision: None,
            },
        )
    })
    .collect();
    result.records.push(RecordContribution {
        identity, record_type: "calendar-invite".into(),
        updates: vec![SemanticUpdate { key: update_key, occurred_at: occurred, summary: format!("{} · {}", e.title, e.start), notify: true, evidence: vec![source.clone()], fields: fields.keys().cloned().collect(), supersedes: None }],
        fields, attention: vec![], links: vec![], presentation: json!({"family":"reservation","version":1,"title":e.title,"subtitle":e.location,"eventAt":e.start}), lifecycle: json!({"state":"active"}),
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(name: &str) -> EmailInput {
        let text = match name {
            "ics" => include_str!("../fixtures/ics-input.json"),
            _ => include_str!("../fixtures/text-input.json"),
        };
        serde_json::from_str(text).unwrap()
    }
    #[test]
    fn structured_and_text_invites_agree() {
        assert_eq!(
            input("ics").calendar_attachments[0]
                .content
                .replace("\r\n", "\n"),
            include_str!("../fixtures/invite.ics")
        );
        let a = extract(input("ics"));
        let b = extract(input("text"));
        assert_eq!(a.records.len(), 1);
        assert_eq!(json!(a.records), json!(b.records));
        assert_eq!(a.records[0].fields["eventTitle"].value, "Planning, review");
        assert_eq!(a.records[0].fields["startAt"].value, "2026-11-01T15:00:00Z");
        assert_eq!(a.records[0].fields["endAt"].value, "2026-11-01T16:00:00Z");
        assert_eq!(a.records[0].fields["location"].value, "Room 2; HQ");
    }
    #[test]
    fn unsupported_and_ambiguous_inputs_remain_mail() {
        let original = input("ics").calendar_attachments[0].content.clone();
        for body in [
            original.replace("20261101T150000Z", "20261101T150000"),
            original.replace("VERSION:2.0", "VERSION:2.0\r\nmethod:cancel"),
            original.replace("DTSTART:", "DTSTART;TZID=America/New_York:"),
            original.replace("20261101T160000Z", "20261101T140000Z"),
            original.replace("20261101T150000Z", "20260230T150000Z"),
            original.replace("END:VEVENT", "RRULE:FREQ=WEEKLY\nEND:VEVENT"),
            original.replace("END:VEVENT", "SUMMARY:Other\nEND:VEVENT"),
            original.replace("END:VCALENDAR", "BEGIN:VEVENT\nEND:VEVENT\nEND:VCALENDAR"),
        ] {
            let mut i = input("ics");
            i.calendar_attachments[0].content = body;
            assert!(extract(i).records.is_empty());
        }
        let mut i = input("text");
        i.text_body = Some("You're invited!".into());
        assert!(extract(i).records.is_empty());
        let mut i = input("text");
        i.message.source_id = Some("unapproved".into());
        assert!(extract(i).records.is_empty());
    }
}
