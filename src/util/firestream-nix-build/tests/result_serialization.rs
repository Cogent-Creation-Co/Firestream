//! Result-file JSON / JUnit serialization shape tests. The Python tool's
//! consumers (CI summary writers, downstream parsers) depend on the exact
//! shape `{"results": [{"attr","duration","error",("outputs"),"success","type"}]}`
//! with alphabetical key ordering.

use std::collections::BTreeMap;

use firestream_nix_build::result::{Outcome, ResultKind, dump_json, dump_junit_xml};

fn fixture() -> Vec<Outcome> {
    vec![
        Outcome {
            kind: ResultKind::Eval,
            attr: "checks.x86_64-linux.foo".into(),
            success: true,
            duration: 0.0,
            error: None,
            log_output: None,
            outputs: None,
        },
        Outcome {
            kind: ResultKind::Build,
            attr: "checks.x86_64-linux.foo".into(),
            success: false,
            duration: 1.5,
            error: Some("build exited with 1".into()),
            log_output: Some("oops".into()),
            outputs: Some({
                let mut m = BTreeMap::new();
                m.insert("out".to_string(), "/nix/store/...-foo".to_string());
                m
            }),
        },
    ]
}

#[test]
fn json_shape_matches_python() {
    let mut buf = Vec::<u8>::new();
    dump_json(&mut buf, &fixture()).unwrap();
    let s = String::from_utf8(buf).unwrap();
    // Keys are emitted alphabetically: attr, duration, error, outputs, success, type.
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let arr = v.get("results").unwrap().as_array().unwrap();
    assert_eq!(arr.len(), 2);

    let eval = &arr[0];
    assert_eq!(eval.get("type").unwrap(), "EVAL");
    assert_eq!(eval.get("success").unwrap(), true);
    assert!(eval.get("outputs").is_none(), "outputs absent when None");

    let build = &arr[1];
    assert_eq!(build.get("type").unwrap(), "BUILD");
    assert_eq!(build.get("success").unwrap(), false);
    assert_eq!(
        build
            .get("outputs")
            .unwrap()
            .get("out")
            .unwrap()
            .as_str()
            .unwrap(),
        "/nix/store/...-foo"
    );
}

#[test]
fn junit_shape_matches_python() {
    let mut buf = Vec::<u8>::new();
    dump_junit_xml(&mut buf, ".#checks", &fixture()).unwrap();
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("<testsuites>"));
    assert!(s.contains("<testsuite name=\".#checks\" tests=\"2\" failures=\"1\">"));
    assert!(s.contains("<testcase classname=\"Build\""));
    assert!(s.contains("<failure message=\"build exited with 1\" type=\"BuildFailure\">"));
}
