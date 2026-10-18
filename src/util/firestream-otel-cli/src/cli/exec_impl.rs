//! Helpers backing `cli::exec::run`. Factored out so the tricky bits — string
//! substitution, attribute assembly, child-process construction — are easy to
//! unit-test without standing up a real exec.
//!
//! Go reference: `otelcli/exec.go` (functions `processArgAttrs`,
//! `processPidAttrs`, and the bits of `doExec` that build the child).

use std::path::Path;
use std::process::Stdio;

use opentelemetry_proto::tonic::common::v1::{
    any_value::Value as AnyValueOneof, AnyValue, ArrayValue, KeyValue,
};

/// Replace literal `{{traceparent}}` substrings in every arg with `tp_str`.
///
/// Mirrors `exec.go::doExec` lines 99-103. Operates in-place over the slice;
/// no-op if no placeholder is present.
pub fn inject_traceparent(args: &mut [String], tp_str: &str) {
    for a in args.iter_mut() {
        if a.contains("{{traceparent}}") {
            *a = a.replace("{{traceparent}}", tp_str);
        }
    }
}

/// Build the `tokio::process::Command` for the child:
/// - inherits stdio from the parent so interactive programs work
/// - inherits env, then sets `TRACEPARENT=<tp_str>` unless `disable_inject`
/// - applies `{{traceparent}}` substitution to args when not disabled
///
/// Mirrors `exec.go::doExec` lines 93-125.
pub fn build_child_command(
    program: &str,
    args: &[String],
    tp_str: &str,
    disable_inject: bool,
) -> tokio::process::Command {
    let mut owned: Vec<String> = args.to_vec();
    if !disable_inject {
        inject_traceparent(&mut owned, tp_str);
    }

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&owned);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    if !disable_inject {
        cmd.env("TRACEPARENT", tp_str);
    } else {
        // Mirror Go's behaviour: when injection is disabled the child does NOT
        // inherit our TRACEPARENT (which is a fresh one we just built).
        cmd.env_remove("TRACEPARENT");
    }

    cmd
}

/// Build OTel process.* attributes for a child invocation.
///
/// `full_args[0]` is the program; `full_args[1..]` are the actual arguments.
/// Mirrors `exec.go::processArgAttrs` + `processPidAttrs`, plus the
/// `process.executable.name` and `process.command_line` attrs the otel-cli-rs
/// task list calls out.
pub fn process_attributes(
    program: &str,
    full_args: &[String],
    pid: u32,
    ppid: u32,
) -> Vec<KeyValue> {
    let mut out = Vec::with_capacity(7);

    out.push(string_kv("process.command", program));

    // process.command_args = array of every arg AFTER the program name. Go
    // uses the entire args slice including args[0]; the OTel spec says
    // command_args should NOT include argv[0] (which is the executable). We
    // emit the spec-aligned version (args[1..]); if Go ever round-trips this
    // back the discrepancy is harmless because the program is already in
    // process.command.
    let tail: Vec<AnyValue> = full_args
        .iter()
        .skip(1)
        .map(|a| AnyValue {
            value: Some(AnyValueOneof::StringValue(a.clone())),
        })
        .collect();
    out.push(KeyValue {
        key: "process.command_args".to_string(),
        value: Some(AnyValue {
            value: Some(AnyValueOneof::ArrayValue(ArrayValue { values: tail })),
        }),
        ..Default::default()
    });

    // Full command line — space-joined, including args[0].
    let cmd_line = full_args.join(" ");
    out.push(string_kv("process.command_line", &cmd_line));

    // Basename of the program path.
    let exe_name = Path::new(program)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string());
    out.push(string_kv("process.executable.name", &exe_name));

    out.push(string_kv("process.owner", &whoami::username()));
    out.push(int_kv("process.pid", pid as i64));
    out.push(int_kv("process.parent_pid", ppid as i64));

    out
}

fn string_kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(AnyValueOneof::StringValue(value.to_string())),
        }),
        ..Default::default()
    }
}

fn int_kv(key: &str, value: i64) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(AnyValueOneof::IntValue(value)),
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TP: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

    #[test]
    fn inject_traceparent_replaces_literal() {
        let mut args = vec!["--header".into(), "tp={{traceparent}}".into()];
        inject_traceparent(&mut args, TP);
        assert_eq!(args[1], format!("tp={TP}"));
    }

    #[test]
    fn inject_traceparent_with_no_placeholder_is_noop() {
        let original = vec!["arg1".to_string(), "arg2".to_string()];
        let mut args = original.clone();
        inject_traceparent(&mut args, TP);
        assert_eq!(args, original);
    }

    #[test]
    fn inject_traceparent_multiple_args() {
        let mut args = vec![
            "{{traceparent}}".into(),
            "untouched".into(),
            "x={{traceparent}}".into(),
        ];
        inject_traceparent(&mut args, TP);
        assert_eq!(args[0], TP);
        assert_eq!(args[1], "untouched");
        assert_eq!(args[2], format!("x={TP}"));
    }

    #[test]
    fn process_attributes_populated() {
        let attrs = process_attributes(
            "/usr/bin/curl",
            &[
                "/usr/bin/curl".to_string(),
                "-X".to_string(),
                "POST".to_string(),
                "https://example".to_string(),
            ],
            42,
            7,
        );
        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();
        assert!(keys.contains(&"process.command"));
        assert!(keys.contains(&"process.command_args"));
        assert!(keys.contains(&"process.command_line"));
        assert!(keys.contains(&"process.executable.name"));
        assert!(keys.contains(&"process.owner"));
        assert!(keys.contains(&"process.pid"));
        assert!(keys.contains(&"process.parent_pid"));

        // process.command = the program (args[0])
        let cmd = attrs.iter().find(|kv| kv.key == "process.command").unwrap();
        match cmd.value.as_ref().unwrap().value.as_ref().unwrap() {
            AnyValueOneof::StringValue(s) => assert_eq!(s, "/usr/bin/curl"),
            other => panic!("unexpected: {other:?}"),
        }

        // process.pid = 42 as int
        let pid = attrs.iter().find(|kv| kv.key == "process.pid").unwrap();
        match pid.value.as_ref().unwrap().value.as_ref().unwrap() {
            AnyValueOneof::IntValue(i) => assert_eq!(*i, 42),
            other => panic!("unexpected: {other:?}"),
        }

        // process.command_args has the three tail args
        let args = attrs
            .iter()
            .find(|kv| kv.key == "process.command_args")
            .unwrap();
        match args.value.as_ref().unwrap().value.as_ref().unwrap() {
            AnyValueOneof::ArrayValue(arr) => assert_eq!(arr.values.len(), 3),
            other => panic!("unexpected: {other:?}"),
        }

        // process.command_line is space-joined including argv[0]
        let cmdline = attrs
            .iter()
            .find(|kv| kv.key == "process.command_line")
            .unwrap();
        match cmdline.value.as_ref().unwrap().value.as_ref().unwrap() {
            AnyValueOneof::StringValue(s) => {
                assert_eq!(s, "/usr/bin/curl -X POST https://example")
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn process_attributes_uses_basename_for_executable_name() {
        let attrs = process_attributes("/usr/bin/curl", &["/usr/bin/curl".to_string()], 1, 1);
        let exe = attrs
            .iter()
            .find(|kv| kv.key == "process.executable.name")
            .unwrap();
        match exe.value.as_ref().unwrap().value.as_ref().unwrap() {
            AnyValueOneof::StringValue(s) => assert_eq!(s, "curl"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn build_child_command_sets_traceparent_env_when_enabled() {
        let cmd = build_child_command("echo", &["hi".to_string()], TP, false);
        let envs: Vec<(String, String)> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(envs
            .iter()
            .any(|(k, v)| k == "TRACEPARENT" && v == TP));
    }
}
