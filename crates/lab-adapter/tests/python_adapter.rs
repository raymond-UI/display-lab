//! The Python adapter template, as a real child process. Proves the
//! protocol works across languages and across a process boundary, not just
//! between two Rust threads. Skipped where there is no `python3`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lab_adapter::{Adapter, AdapterError, RpcAdapter};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

#[test]
fn the_python_template_speaks_the_protocol() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: no python3");
        return;
    }
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../adapters/example-python/adapter.py");
    let mut app = RpcAdapter::spawn(&["python3".into(), script.display().to_string()]).unwrap();
    assert_eq!(app.info().name, "example-python");
    assert!(app.info().capabilities.state);
    assert!(!app.info().capabilities.expected_frame);

    app.launch(&[]).unwrap();
    app.input("next", &Value::Null).unwrap();
    app.input("next", &Value::Null).unwrap();
    assert_eq!(app.state().unwrap()["slide"], json!(3));

    assert!(matches!(
        app.input("jump", &Value::Null),
        Err(AdapterError::Remote { .. })
    ));
    assert!(matches!(
        app.bindings(),
        Err(AdapterError::Unsupported("bindings"))
    ));
    app.stop().unwrap();
}
