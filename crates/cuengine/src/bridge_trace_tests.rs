//! Raw tracing subscribers must never receive CUE payloads or diagnostics.

use std::fmt::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<String>>);

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut capture = self.clone();
        event.record(&mut capture);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

impl Visit for Capture {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if let Ok(mut captured) = self.0.lock() {
            let _ = writeln!(captured, "{}={value:?}", field.name());
        }
    }
}

#[test]
fn raw_bridge_tracing_excludes_payloads_error_messages_and_hints() {
    let capture = Capture::default();
    tracing::subscriber::with_default(capture.clone(), || {
        let _ = super::parse_bridge_envelope(
            r#"{"version":"secret-in-envelope","extra":["x"],broken}"#,
        );
        let _ =
            super::parse_module_result(r#"{"instances":"secret-in-payload","x":true,"n":13.25}"#);
        let _ = super::handle_bridge_error(
            super::BridgeError {
                code: super::ERROR_CODE_BUILD_VALUE.to_owned(),
                message: "secret-in-error: conflicting x, true and 13.25".to_owned(),
                hint: Some("secret-in-hint".to_owned()),
            },
            Path::new("."),
        );
    });
    let captured = capture.0.lock().expect("trace capture lock poisoned");
    assert!(captured.contains("response_bytes"));
    assert!(captured.contains("error_code"));
    for forbidden in [
        "secret-in-",
        "conflicting",
        "13.25",
        "true",
        "json_response",
        "json_data",
        "parse_error",
        "error_message",
        "error_hint",
    ] {
        assert!(
            !captured.contains(forbidden),
            "raw trace contained {forbidden}"
        );
    }
}

#[test]
fn poisoned_capture_does_not_panic_while_recording() {
    let capture = Capture::default();
    tracing::subscriber::with_default(capture.clone(), || {
        tracing::info!(probe = "before");
    });
    assert!(
        capture
            .0
            .lock()
            .expect("capture starts healthy")
            .contains("before")
    );

    let poisoner = Arc::clone(&capture.0);
    assert!(
        std::thread::spawn(move || {
            let _guard = poisoner.lock().expect("capture starts healthy");
            panic!("poison trace capture");
        })
        .join()
        .is_err()
    );
    tracing::subscriber::with_default(capture.clone(), || {
        tracing::info!(probe = "after");
    });
    assert!(capture.0.is_poisoned());
}

#[test]
fn public_evaluation_timeout_uses_the_existing_validated_setting() {
    let expected =
        super::parse_module_eval_timeout(std::env::var_os(super::EVAL_TIMEOUT_ENV_VAR).as_deref());
    assert_eq!(
        super::evaluation_timeout().map_err(|error| error.to_string()),
        expected.map_err(|error| error.to_string())
    );
}
