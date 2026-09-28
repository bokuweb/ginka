//! N6: a file the user attaches is stored by the daemon and reaches the agent
//! as something it can open.

use ginka_core::service::{EventSink, Service};
use ginka_core::{Paths, db};
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::rpc::{Request, Response};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Silent {
    events: Mutex<Vec<DaemonEvent>>,
}

impl EventSink for Silent {
    fn emit(&self, event: DaemonEvent) {
        self.events.lock().unwrap().push(event);
    }
}

struct Fixture {
    service: Service,
    paths: Paths,
    _home: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(home.path().join("state"));
        paths.ensure().unwrap();
        let service = Service::new(
            paths.clone(),
            db::open_in_memory().unwrap(),
            Arc::new(Silent::default()),
        );
        Self {
            service,
            paths,
            _home: home,
        }
    }

    fn upload(&mut self, name: &str, bytes: &[u8]) -> Response {
        use base64::Engine as _;
        self.service
            .handle(Request::UploadAttachment {
                name: name.into(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
            .unwrap_or_else(|error| panic!("upload failed: {error}"))
    }
}

fn stored(response: Response) -> ginka_protocol::model::Attachment {
    match response {
        Response::Attachment { attachment } => attachment,
        other => panic!("expected an attachment, got {other:?}"),
    }
}

#[test]
fn uploaded_images_can_be_read_through_the_daemon_with_signature_checked_preview() {
    let mut fixture = Fixture::new();
    let png = b"\x89PNG\r\n\x1a\npreview";
    let image = stored(fixture.upload("chart.bin", png));
    let response = fixture
        .service
        .handle(Request::ReadAttachmentImage {
            reference: image.reference,
        })
        .unwrap();
    let Response::AttachmentImage { image: Some(image) } = response else {
        panic!("expected an image preview: {response:?}");
    };
    assert_eq!(image.media_type, "image/png");
    use base64::Engine as _;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(image.data_base64)
            .unwrap(),
        png
    );
}

#[test]
fn attachment_image_read_refuses_missing_non_image_and_unsafe_references() {
    let mut fixture = Fixture::new();
    let text = stored(fixture.upload("readme.txt", b"not an image"));
    let mut oversized = vec![0; ginka_core::files::IMAGE_PREVIEW_LIMIT + 1];
    oversized[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let oversized = stored(fixture.upload("too-large.png", &oversized));
    for reference in [
        text.reference,
        oversized.reference,
        "ginka-attachment:../secret".into(),
        "ginka-attachment:missing".into(),
    ] {
        let response = fixture
            .service
            .handle(Request::ReadAttachmentImage { reference })
            .unwrap();
        assert!(matches!(
            response,
            Response::AttachmentImage { image: None }
        ));
    }
}

#[cfg(unix)]
#[test]
fn attachment_image_read_refuses_symlinks_outside_the_store() {
    let mut fixture = Fixture::new();
    let attachment = stored(fixture.upload("chart.png", b"\x89PNG\r\n\x1a\ninside"));
    let path = ginka_core::attachment::AttachmentStore::new(fixture.paths.attachments())
        .path_of(&attachment.reference)
        .unwrap();
    let outside = fixture._home.path().join("outside.png");
    std::fs::write(&outside, b"\x89PNG\r\n\x1a\noutside").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();

    let response = fixture
        .service
        .handle(Request::ReadAttachmentImage {
            reference: attachment.reference,
        })
        .unwrap();
    assert!(matches!(
        response,
        Response::AttachmentImage { image: None }
    ));
}

#[test]
fn an_upload_answers_with_a_reference_the_daemon_can_resolve() {
    let mut fixture = Fixture::new();
    let attachment = stored(fixture.upload("notes.md", b"# hello"));

    assert!(attachment.reference.starts_with("ginka-attachment:"));
    assert_eq!(attachment.name, "notes.md");
    assert_eq!(attachment.bytes, 7);
    // The path is the daemon's business (§4.1); what crosses the wire is the
    // reference, and the daemon is what turns it back into a file.
    let store = ginka_core::attachment::AttachmentStore::new(fixture.paths.attachments());
    assert_eq!(
        store.read(&attachment.reference).unwrap().unwrap(),
        b"# hello"
    );
}

#[test]
fn two_files_with_the_same_name_are_two_attachments() {
    let mut fixture = Fixture::new();
    let first = stored(fixture.upload("notes.md", b"first"));
    let second = stored(fixture.upload("notes.md", b"second"));

    assert_ne!(first.reference, second.reference);
    let store = ginka_core::attachment::AttachmentStore::new(fixture.paths.attachments());
    assert_eq!(store.read(&first.reference).unwrap().unwrap(), b"first");
}

#[test]
fn an_oversized_upload_is_refused_with_its_limit_named() {
    let mut fixture = Fixture::new();
    let error = fixture
        .service
        .handle(Request::UploadAttachment {
            name: "huge.bin".into(),
            data_base64: {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(vec![
                    0u8;
                    ginka_core::attachment::MAX_ATTACHMENT_BYTES
                        + 1
                ])
            },
        })
        .unwrap_err();
    assert!(error.message.contains("too large"), "{error:?}");
}

#[test]
fn an_empty_upload_is_refused() {
    let mut fixture = Fixture::new();
    let error = fixture
        .service
        .handle(Request::UploadAttachment {
            name: "empty.bin".into(),
            data_base64: String::new(),
        })
        .unwrap_err();
    assert!(!error.message.is_empty());
}

#[test]
fn a_payload_that_is_not_base64_is_refused_rather_than_stored_as_rubbish() {
    let mut fixture = Fixture::new();
    let error = fixture
        .service
        .handle(Request::UploadAttachment {
            name: "notes.md".into(),
            data_base64: "!!! not base64 !!!".into(),
        })
        .unwrap_err();
    assert!(error.message.to_lowercase().contains("base64"), "{error:?}");
}

#[test]
fn a_message_that_mentions_an_attachment_carries_a_path_the_agent_can_open() {
    let mut fixture = Fixture::new();
    let attachment = stored(fixture.upload("notes.md", b"# hello"));
    let store = ginka_core::attachment::AttachmentStore::new(fixture.paths.attachments());

    let expanded = ginka_core::attachment::expand_references(
        &format!("read {} and summarise it", attachment.reference),
        &store,
    );
    let path = store.path_of(&attachment.reference).unwrap();
    assert!(
        expanded.contains(&path.to_string_lossy().to_string()),
        "{expanded}"
    );
    assert!(!expanded.contains("ginka-attachment:"), "{expanded}");
}

#[test]
fn a_reference_to_something_that_is_not_there_is_left_alone() {
    // Silently replacing it with nothing would leave the agent reading a
    // sentence with a hole in it and no way to say so.
    let tmp = tempfile::tempdir().unwrap();
    let store = ginka_core::attachment::AttachmentStore::new(tmp.path());
    let text = "read ginka-attachment:nope please";
    assert_eq!(
        ginka_core::attachment::expand_references(text, &store),
        text
    );
}

#[test]
fn a_reference_that_tries_to_escape_the_store_is_not_expanded() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ginka_core::attachment::AttachmentStore::new(tmp.path());
    let text = "read ginka-attachment:../../etc/passwd";
    assert_eq!(
        ginka_core::attachment::expand_references(text, &store),
        text
    );
}

#[test]
fn text_with_no_attachments_is_returned_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ginka_core::attachment::AttachmentStore::new(tmp.path());
    let text = "just a prompt";
    assert_eq!(
        ginka_core::attachment::expand_references(text, &store),
        text
    );
}
