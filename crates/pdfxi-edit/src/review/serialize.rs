//! Dictionary round-trip for review registries.
//!
//! This module maps a [`ReviewRegistry`] to and from the dependency-free
//! [`Dict`]/[`Value`] intermediate defined in [`crate::pdf`]. All keys owned by
//! this module live in a private `XI_Thread...` namespace so they can never
//! collide with standard PDF annotation keys:
//!
//! - Registry: `XI_ThreadVersion`, `XI_ThreadItems`.
//! - Thread: `XI_ThreadId`, `XI_ThreadRoot`, `XI_ThreadPage`, `XI_ThreadStatus`,
//!   `XI_ThreadAssignee`, `XI_ThreadCreated`, `XI_ThreadUpdated`,
//!   `XI_ThreadReplies`, `XI_ThreadHistory`.
//! - Reply: `XI_ThreadReplyId`, `XI_ThreadAuthor`, `XI_ThreadTime`,
//!   `XI_ThreadBody`, `XI_ThreadStatus`.
//! - History: `XI_ThreadFrom`, `XI_ThreadTo`, `XI_ThreadTime`, `XI_ThreadActor`,
//!   `XI_ThreadNote`.
//!
//! Keys that are not in this namespace are preserved verbatim in the `extra`
//! lists of the corresponding objects. Because [`Dict`] is a `BTreeMap`, both
//! key ordering and thread ordering are deterministic.

use crate::annotation::AnnotationId;
use crate::geometry::PageIndex;
use crate::pdf::{Dict, Value};

use super::error::ReviewError;
use super::registry::ReviewRegistry;
use super::thread::{CommentThread, Reply, ReplyId, ThreadId};
use super::workflow::{HistoryEntry, ReviewState, ReviewStatus};

const REGISTRY_KEYS: &[&str] = &["XI_ThreadVersion", "XI_ThreadItems"];
const THREAD_KEYS: &[&str] = &[
    "XI_ThreadId",
    "XI_ThreadRoot",
    "XI_ThreadPage",
    "XI_ThreadStatus",
    "XI_ThreadAssignee",
    "XI_ThreadCreated",
    "XI_ThreadUpdated",
    "XI_ThreadReplies",
    "XI_ThreadHistory",
];
const REPLY_KEYS: &[&str] = &[
    "XI_ThreadReplyId",
    "XI_ThreadAuthor",
    "XI_ThreadTime",
    "XI_ThreadBody",
    "XI_ThreadStatus",
];
const HISTORY_KEYS: &[&str] = &[
    "XI_ThreadFrom",
    "XI_ThreadTo",
    "XI_ThreadTime",
    "XI_ThreadActor",
    "XI_ThreadNote",
];

fn id_value(id: u64) -> Value {
    Value::Int(i64::try_from(id).unwrap_or(i64::MAX))
}

fn read_id(dict: &Dict, key: &'static str) -> Result<u64, ReviewError> {
    let value = dict.get(key).ok_or(ReviewError::MissingKey(key))?;
    let number = value.as_int().ok_or_else(|| ReviewError::WrongType {
        key: key.to_string(),
        expected: "an integer id",
    })?;
    u64::try_from(number).map_err(|_| ReviewError::NegativeId(number))
}

fn read_text(dict: &Dict, key: &'static str) -> Result<Option<String>, ReviewError> {
    match dict.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_text()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| ReviewError::WrongType {
                key: key.to_string(),
                expected: "a string",
            }),
    }
}

fn required_text(dict: &Dict, key: &'static str) -> Result<String, ReviewError> {
    read_text(dict, key)?.ok_or(ReviewError::MissingKey(key))
}

fn read_name(dict: &Dict, key: &'static str) -> Result<String, ReviewError> {
    let value = dict.get(key).ok_or(ReviewError::MissingKey(key))?;
    value
        .as_name()
        .map(str::to_string)
        .ok_or_else(|| ReviewError::WrongType {
            key: key.to_string(),
            expected: "a name",
        })
}

fn read_status(dict: &Dict, key: &'static str) -> Result<ReviewStatus, ReviewError> {
    let name = read_name(dict, key)?;
    ReviewStatus::from_name(&name).ok_or(ReviewError::UnknownStatus(name))
}

fn read_array<'a>(dict: &'a Dict, key: &'static str) -> Result<&'a [Value], ReviewError> {
    let value = dict.get(key).ok_or(ReviewError::MissingKey(key))?;
    value.as_array().ok_or_else(|| ReviewError::WrongType {
        key: key.to_string(),
        expected: "an array",
    })
}

fn collect_extra(dict: &Dict, known: &[&str]) -> Vec<(String, Value)> {
    dict.iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn merge_extra(dict: &mut Dict, extra: &[(String, Value)]) {
    for (key, value) in extra {
        if !dict.contains_key(key) {
            dict.insert(key.clone(), value.clone());
        }
    }
}

fn reply_to_dict(reply: &Reply) -> Dict {
    let mut dict = Dict::new();
    dict.insert("XI_ThreadReplyId".to_string(), id_value(reply.id().get()));
    dict.insert(
        "XI_ThreadAuthor".to_string(),
        Value::Text(reply.author().to_string()),
    );
    dict.insert(
        "XI_ThreadTime".to_string(),
        Value::Text(reply.timestamp().to_string()),
    );
    dict.insert(
        "XI_ThreadBody".to_string(),
        Value::Text(reply.body().to_string()),
    );
    dict.insert(
        "XI_ThreadStatus".to_string(),
        Value::Name(reply.status().as_str().to_string()),
    );
    merge_extra(&mut dict, reply.extra());
    dict
}

fn reply_from_dict(dict: &Dict) -> Result<Reply, ReviewError> {
    let id = ReplyId::new(read_id(dict, "XI_ThreadReplyId")?);
    let author = required_text(dict, "XI_ThreadAuthor")?;
    let timestamp = required_text(dict, "XI_ThreadTime")?;
    let body = required_text(dict, "XI_ThreadBody")?;
    let status = read_status(dict, "XI_ThreadStatus")?;
    Ok(Reply::new(id, author, timestamp, body, status).with_extra(collect_extra(dict, REPLY_KEYS)))
}

fn history_to_dict(entry: &HistoryEntry) -> Dict {
    let mut dict = Dict::new();
    match entry.from() {
        Some(status) => dict.insert(
            "XI_ThreadFrom".to_string(),
            Value::Name(status.as_str().to_string()),
        ),
        None => dict.insert("XI_ThreadFrom".to_string(), Value::Null),
    };
    dict.insert(
        "XI_ThreadTo".to_string(),
        Value::Name(entry.to().as_str().to_string()),
    );
    dict.insert(
        "XI_ThreadTime".to_string(),
        Value::Text(entry.timestamp().to_string()),
    );
    dict.insert(
        "XI_ThreadActor".to_string(),
        Value::Text(entry.actor().to_string()),
    );
    dict.insert(
        "XI_ThreadNote".to_string(),
        Value::Text(entry.note().to_string()),
    );
    merge_extra(&mut dict, entry.extra());
    dict
}

fn history_from_dict(dict: &Dict) -> Result<HistoryEntry, ReviewError> {
    let from = match dict.get("XI_ThreadFrom") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let name = value.as_name().ok_or_else(|| ReviewError::WrongType {
                key: "XI_ThreadFrom".to_string(),
                expected: "a name or null",
            })?;
            Some(
                ReviewStatus::from_name(name)
                    .ok_or_else(|| ReviewError::UnknownStatus(name.to_string()))?,
            )
        }
    };
    let to = read_status(dict, "XI_ThreadTo")?;
    let timestamp = read_text(dict, "XI_ThreadTime")?.unwrap_or_default();
    let actor = read_text(dict, "XI_ThreadActor")?.unwrap_or_default();
    let note = read_text(dict, "XI_ThreadNote")?.unwrap_or_default();
    Ok(HistoryEntry::new(from, to, timestamp, actor, note)
        .with_extra(collect_extra(dict, HISTORY_KEYS)))
}

fn thread_to_dict(thread: &CommentThread, page: PageIndex) -> Dict {
    let mut dict = Dict::new();
    dict.insert("XI_ThreadId".to_string(), id_value(thread.id().get()));
    dict.insert("XI_ThreadRoot".to_string(), id_value(thread.root().get()));
    dict.insert("XI_ThreadPage".to_string(), id_value(page.get() as u64));
    dict.insert(
        "XI_ThreadStatus".to_string(),
        Value::Name(thread.status().as_str().to_string()),
    );
    if let Some(assignee) = thread.assignee() {
        dict.insert(
            "XI_ThreadAssignee".to_string(),
            Value::Text(assignee.to_string()),
        );
    }
    dict.insert(
        "XI_ThreadCreated".to_string(),
        Value::Text(thread.created().to_string()),
    );
    dict.insert(
        "XI_ThreadUpdated".to_string(),
        Value::Text(thread.updated().to_string()),
    );
    let replies: Vec<Value> = thread
        .replies()
        .iter()
        .map(|reply| Value::Dictionary(reply_to_dict(reply)))
        .collect();
    dict.insert("XI_ThreadReplies".to_string(), Value::Array(replies));
    let history: Vec<Value> = thread
        .history()
        .iter()
        .map(|entry| Value::Dictionary(history_to_dict(entry)))
        .collect();
    dict.insert("XI_ThreadHistory".to_string(), Value::Array(history));
    merge_extra(&mut dict, thread.extra());
    dict
}

fn thread_from_dict(dict: &Dict) -> Result<(CommentThread, PageIndex), ReviewError> {
    let id = ThreadId::new(read_id(dict, "XI_ThreadId")?);
    let root = AnnotationId::new(read_id(dict, "XI_ThreadRoot")?);
    let page_value = read_id(dict, "XI_ThreadPage")?;
    let page = usize::try_from(page_value)
        .map(PageIndex::new)
        .map_err(|_| ReviewError::IdTooLarge(page_value))?;
    let status = read_status(dict, "XI_ThreadStatus")?;
    let assignee = read_text(dict, "XI_ThreadAssignee")?;
    let created = required_text(dict, "XI_ThreadCreated")?;
    let updated = required_text(dict, "XI_ThreadUpdated")?;

    let reply_values = read_array(dict, "XI_ThreadReplies")?;
    if reply_values.is_empty() {
        return Err(ReviewError::EmptyThread(id));
    }
    let mut replies = Vec::with_capacity(reply_values.len());
    for value in reply_values {
        let reply_dict = value.as_dict().ok_or_else(|| ReviewError::WrongType {
            key: "XI_ThreadReplies".to_string(),
            expected: "an array of dictionaries",
        })?;
        replies.push(reply_from_dict(reply_dict)?);
    }

    let mut history = Vec::new();
    if let Some(values) = dict
        .get("XI_ThreadHistory")
        .map(|value| {
            value.as_array().ok_or_else(|| ReviewError::WrongType {
                key: "XI_ThreadHistory".to_string(),
                expected: "an array",
            })
        })
        .transpose()?
    {
        for value in values {
            let entry_dict = value.as_dict().ok_or_else(|| ReviewError::WrongType {
                key: "XI_ThreadHistory".to_string(),
                expected: "an array of dictionaries",
            })?;
            history.push(history_from_dict(entry_dict)?);
        }
    }

    let state = ReviewState::from_parts(status, assignee, created, updated, history);
    let thread = CommentThread::from_parts(id, root, replies, state)
        .with_extra(collect_extra(dict, THREAD_KEYS));
    Ok((thread, page))
}

/// Serializes a review registry into a dictionary.
///
/// The result is deterministic: threads are written in root-annotation order,
/// replies in canonical order, and history in chronological order.
pub fn registry_to_dict(registry: &ReviewRegistry) -> Dict {
    let items: Vec<Value> = registry
        .threads()
        .map(|thread| {
            let page = registry.page_of(thread.root()).unwrap_or(PageIndex::new(0));
            Value::Dictionary(thread_to_dict(thread, page))
        })
        .collect();
    let mut dict = Dict::new();
    dict.insert("XI_ThreadVersion".to_string(), Value::Int(1));
    dict.insert("XI_ThreadItems".to_string(), Value::Array(items));
    merge_extra(&mut dict, registry.extra());
    dict
}

/// Parses a dictionary produced by [`registry_to_dict`] back into a registry.
///
/// Duplicate thread ids, duplicate roots, empty threads, unknown statuses, and
/// malformed values are all reported as structured [`ReviewError`]s.
pub fn dict_to_registry(dict: &Dict) -> Result<ReviewRegistry, ReviewError> {
    let mut threads = Vec::new();
    for value in read_array(dict, "XI_ThreadItems")? {
        let thread_dict = value.as_dict().ok_or_else(|| ReviewError::WrongType {
            key: "XI_ThreadItems".to_string(),
            expected: "an array of dictionaries",
        })?;
        threads.push(thread_from_dict(thread_dict)?);
    }
    ReviewRegistry::from_parsed(threads, collect_extra(dict, REGISTRY_KEYS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Annotation, AnnotationKind};
    use crate::geometry::Rect;
    use crate::review::workflow::ReviewStatus;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 10.0, 10.0).expect("valid rect")
    }

    fn annotation(id: u64, page: usize) -> Annotation {
        Annotation::new(
            AnnotationId::new(id),
            AnnotationKind::Highlight,
            PageIndex::new(page),
            rect(),
            "author",
            "body",
        )
        .expect("valid annotation")
    }

    fn populated() -> ReviewRegistry {
        let annotations = [annotation(2, 1), annotation(1, 0)];
        let mut registry = ReviewRegistry::from_annotations(annotations.iter(), "t0")
            .expect("annotations must group");
        registry
            .add_reply(
                AnnotationId::new(1),
                "alice",
                "t1",
                "first",
                ReviewStatus::Open,
            )
            .expect("reply 1");
        registry
            .add_reply(
                AnnotationId::new(2),
                "bob",
                "t2",
                "second",
                ReviewStatus::Open,
            )
            .expect("reply 2");
        registry
            .transition(
                AnnotationId::new(1),
                ReviewStatus::Resolved,
                "alice",
                "t3",
                "done",
            )
            .expect("resolve");
        registry
    }

    #[test]
    fn registry_round_trip_preserves_threads_replies_and_history() {
        let registry = populated();
        let dict = registry_to_dict(&registry);
        assert_eq!(
            dict.get("XI_ThreadVersion").and_then(Value::as_int),
            Some(1)
        );
        let restored = dict_to_registry(&dict).expect("round-trip");
        assert_eq!(restored, registry);
        assert_eq!(restored.summary(), registry.summary());
        assert_eq!(
            restored.page_of(AnnotationId::new(2)),
            Some(PageIndex::new(1))
        );
    }

    #[test]
    fn serialization_is_deterministic_and_uses_private_namespace() {
        let registry = populated();
        let first = registry_to_dict(&registry);
        assert_eq!(first, registry_to_dict(&registry));
        let items = first
            .get("XI_ThreadItems")
            .and_then(Value::as_array)
            .expect("items");
        let thread = items[0].as_dict().expect("thread dict");
        assert!(thread.keys().all(|key| key.starts_with("XI_Thread")));
        let replies = thread
            .get("XI_ThreadReplies")
            .and_then(Value::as_array)
            .expect("replies");
        let reply = replies[0].as_dict().expect("reply dict");
        assert!(reply.keys().all(|key| key.starts_with("XI_Thread")));
    }

    #[test]
    fn unknown_keys_are_preserved_at_every_level() {
        let mut registry = populated();
        registry.set_extra(vec![(
            "RegistryTag".to_string(),
            Value::Text("r".to_string()),
        )]);
        if let Some(thread) = registry.thread_mut(AnnotationId::new(1)) {
            thread.set_extra(vec![("ThreadTag".to_string(), Value::Int(11))]);
            let reply_id = thread.replies()[0].id();
            if let Some(reply) = thread.reply_mut(reply_id) {
                reply.set_extra(vec![("ReplyTag".to_string(), Value::Bool(true))]);
            }
        }

        let dict = registry_to_dict(&registry);
        assert_eq!(dict.get("RegistryTag").and_then(Value::as_text), Some("r"));
        let items = dict
            .get("XI_ThreadItems")
            .and_then(Value::as_array)
            .expect("items");
        let thread_dict = items[0].as_dict().expect("thread");
        assert_eq!(
            thread_dict.get("ThreadTag").and_then(Value::as_int),
            Some(11)
        );
        let replies = thread_dict
            .get("XI_ThreadReplies")
            .and_then(Value::as_array)
            .expect("replies");
        assert_eq!(
            replies[0]
                .as_dict()
                .and_then(|d| d.get("ReplyTag"))
                .and_then(Value::as_bool),
            Some(true)
        );

        let restored = dict_to_registry(&dict).expect("round-trip");
        assert_eq!(restored.extra(), registry.extra());
        let thread = restored.thread(AnnotationId::new(1)).expect("thread");
        assert_eq!(
            thread
                .extra()
                .iter()
                .find(|(k, _)| k == "ThreadTag")
                .map(|(_, v)| v),
            Some(&Value::Int(11))
        );
        assert_eq!(
            thread.replies()[0]
                .extra()
                .iter()
                .find(|(k, _)| k == "ReplyTag")
                .map(|(_, v)| v),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn unknown_keys_do_not_overwrite_owned_keys() {
        let registry = populated();
        let mut dict = registry_to_dict(&registry);
        dict.insert(
            "XI_ThreadVersion".to_string(),
            Value::Text("wrong".to_string()),
        );
        // The owned key wins because extra keys are merged only when absent.
        assert_eq!(
            dict.get("XI_ThreadVersion").and_then(Value::as_text),
            Some("wrong")
        );
        // Parsing still succeeds because the version is informational.
        assert!(dict_to_registry(&dict).is_ok());
    }

    #[test]
    fn empty_thread_is_rejected_on_parse() {
        let mut dict = registry_to_dict(&populated());
        let items = dict
            .get_mut("XI_ThreadItems")
            .and_then(Value::as_array_mut)
            .expect("items");
        if let Some(Value::Dictionary(thread)) = items.first_mut() {
            thread.insert("XI_ThreadReplies".to_string(), Value::Array(Vec::new()));
        }
        assert!(matches!(
            dict_to_registry(&dict),
            Err(ReviewError::EmptyThread(_))
        ));
    }

    #[test]
    fn duplicate_ids_and_unknown_statuses_are_rejected_on_parse() {
        let mut duplicate = registry_to_dict(&populated());
        let items = duplicate
            .get_mut("XI_ThreadItems")
            .and_then(Value::as_array_mut)
            .expect("items");
        let first_id = items
            .first()
            .and_then(Value::as_dict)
            .and_then(|d| d.get("XI_ThreadId").cloned())
            .expect("id");
        if let Some(Value::Dictionary(second)) = items.get_mut(1) {
            second.insert("XI_ThreadId".to_string(), first_id);
        }
        assert!(matches!(
            dict_to_registry(&duplicate),
            Err(ReviewError::DuplicateThreadId(_))
        ));

        let mut unknown = registry_to_dict(&populated());
        let items = unknown
            .get_mut("XI_ThreadItems")
            .and_then(Value::as_array_mut)
            .expect("items");
        if let Some(Value::Dictionary(thread)) = items.first_mut() {
            thread.insert(
                "XI_ThreadStatus".to_string(),
                Value::Name("bogus".to_string()),
            );
        }
        assert!(matches!(
            dict_to_registry(&unknown),
            Err(ReviewError::UnknownStatus(_))
        ));
    }

    #[test]
    fn negative_and_missing_keys_are_reported() {
        let mut negative = registry_to_dict(&populated());
        let items = negative
            .get_mut("XI_ThreadItems")
            .and_then(Value::as_array_mut)
            .expect("items");
        if let Some(Value::Dictionary(thread)) = items.first_mut() {
            thread.insert("XI_ThreadRoot".to_string(), Value::Int(-1));
        }
        assert!(matches!(
            dict_to_registry(&negative),
            Err(ReviewError::NegativeId(_))
        ));

        let missing = Dict::new();
        assert_eq!(
            dict_to_registry(&missing),
            Err(ReviewError::MissingKey("XI_ThreadItems"))
        );
    }
}
