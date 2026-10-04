//! Embeddable AWS client views.
//!
//! The standalone `aws` executable mounts [`SqsView`]. A host can mount that
//! same view with Ginka's tokens installed, without depending on the binary.

rust_i18n::i18n!("../../locales", fallback = "en");

mod sqs;

pub use sqs::SqsView;
