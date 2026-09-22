//! N6: agent-emitted images and user uploads live outside the transcript.

use ginka_core::attachment::{AttachmentStore, MAX_ATTACHMENT_BYTES};
use ginka_core::blob::{BlobStore, INLINE_THRESHOLD_BYTES};
use ginka_protocol::AgentEvent;
use ginka_protocol::event::ActivityItem;
use serde_json::json;

fn data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn big_png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(INLINE_THRESHOLD_BYTES + 1_000, 7);
    bytes
}

#[test]
fn a_small_image_is_left_inline() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let url = data_url("image/png", b"tiny");

    // A reference plus a file is not worth it for a favicon.
    assert_eq!(store.externalize(&url).unwrap(), url);
    assert_eq!(store.len().unwrap(), 0);
}

#[test]
fn a_large_image_becomes_a_reference_that_resolves_back_to_its_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let bytes = big_png();

    let reference = store.externalize(&data_url("image/png", &bytes)).unwrap();
    assert!(reference.starts_with("ginka-blob:"), "{reference}");
    assert!(
        reference.ends_with(".png"),
        "the type survives: {reference}"
    );
    assert_eq!(store.read(&reference).unwrap().unwrap(), bytes);
    assert_eq!(store.len().unwrap(), 1);
}

#[test]
fn agent_events_store_large_inline_images_before_the_transcript_sees_them() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let bytes = big_png();
    let url = data_url("image/png", &bytes);
    let mut activity = ActivityItem::from_tool(Some("shot".into()), "screenshot", &json!({}));
    activity.detail = Some(url);
    activity.complete = true;
    let mut event = AgentEvent::ToolResult { activity };

    store.externalize_event(&mut event).unwrap();

    let AgentEvent::ToolResult { activity } = event else {
        unreachable!("the event variant is preserved")
    };
    let reference = activity.detail.unwrap();
    assert!(reference.starts_with("ginka-blob:"), "{reference}");
    assert_eq!(store.read(&reference).unwrap().unwrap(), bytes);
}

#[test]
fn ordinary_agent_text_is_not_rewritten() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let mut event = AgentEvent::TextDelta {
        text: "data URLs are useful".into(),
    };

    store.externalize_event(&mut event).unwrap();

    assert_eq!(
        event,
        AgentEvent::TextDelta {
            text: "data URLs are useful".into()
        }
    );
    assert!(store.is_empty().unwrap());
}

#[test]
fn the_same_screenshot_twice_is_stored_once() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let url = data_url("image/png", &big_png());

    let first = store.externalize(&url).unwrap();
    let second = store.externalize(&url).unwrap();
    assert_eq!(first, second);
    assert_eq!(store.len().unwrap(), 1);
}

#[test]
fn concurrent_sessions_can_store_the_same_screenshot() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let url = data_url("image/png", &big_png());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let workers = (0..2)
        .map(|_| {
            let store = store.clone();
            let url = url.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.externalize(&url)
            })
        })
        .collect::<Vec<_>>();

    barrier.wait();
    let references = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(references[0], references[1]);
    assert_eq!(store.len().unwrap(), 1);
}

#[test]
fn different_payloads_get_different_references() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let mut other = big_png();
    other.push(9);

    let first = store
        .externalize(&data_url("image/png", &big_png()))
        .unwrap();
    let second = store.externalize(&data_url("image/png", &other)).unwrap();
    assert_ne!(first, second);
    assert_eq!(store.len().unwrap(), 2);
}

#[test]
fn an_unknown_media_type_still_stores_with_a_neutral_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    let reference = store
        .externalize(&data_url("application/x-thing", &big_png()))
        .unwrap();
    assert!(reference.ends_with(".bin"), "{reference}");
}

#[test]
fn text_that_is_not_a_data_url_passes_through_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    assert_eq!(
        store.externalize("https://example.com/a.png").unwrap(),
        "https://example.com/a.png"
    );
    assert_eq!(store.len().unwrap(), 0);
}

#[test]
fn a_malformed_data_url_is_reported_rather_than_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    assert!(
        store
            .externalize("data:image/png;base64,!!!not base64!!!")
            .is_err()
    );
}

#[test]
fn an_unknown_reference_reads_as_missing_rather_than_failing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::new(tmp.path());
    assert!(
        store
            .read("ginka-blob:0000000000000000.png")
            .unwrap()
            .is_none()
    );
    // Anything that is not one of ours is not ours to resolve.
    assert!(store.read("https://example.com/a.png").unwrap().is_none());
}

#[test]
fn an_upload_round_trips_through_its_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(tmp.path());

    let stored = store.put("notes.md", b"# hello").unwrap();
    assert!(stored.reference.starts_with("ginka-attachment:"));
    assert_eq!(stored.name, "notes.md");
    assert_eq!(store.read(&stored.reference).unwrap().unwrap(), b"# hello");
}

#[test]
fn two_uploads_with_the_same_name_do_not_clobber_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(tmp.path());

    let first = store.put("notes.md", b"first").unwrap();
    let second = store.put("notes.md", b"second").unwrap();
    assert_ne!(first.reference, second.reference);
    assert_eq!(store.read(&first.reference).unwrap().unwrap(), b"first");
    assert_eq!(store.read(&second.reference).unwrap().unwrap(), b"second");
}

#[test]
fn an_oversized_upload_is_refused_with_its_limit_named() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(tmp.path());
    let error = store
        .put("huge.bin", &vec![0u8; MAX_ATTACHMENT_BYTES + 1])
        .unwrap_err()
        .to_string();
    assert!(error.contains("too large"), "{error}");
}

#[test]
fn an_empty_upload_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(tmp.path());
    assert!(store.put("empty.bin", b"").is_err());
}

#[test]
fn a_traversing_name_cannot_write_outside_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(tmp.path().join("attachments"));

    let stored = store.put("../../escaped.txt", b"nope").unwrap();
    assert!(
        stored.path.starts_with(tmp.path().join("attachments")),
        "{}",
        stored.path.display()
    );
    assert!(!tmp.path().join("escaped.txt").exists());
    // The display name is kept for the transcript; only the path is sanitised.
    assert_eq!(stored.name, "../../escaped.txt");
}

#[test]
fn an_unknown_attachment_reads_as_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(tmp.path());
    assert!(store.read("ginka-attachment:nope").unwrap().is_none());
    assert!(store.read("notes.md").unwrap().is_none());
}
