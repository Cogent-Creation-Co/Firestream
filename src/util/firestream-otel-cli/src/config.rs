//! Runtime configuration.
//!
//! Direct port of Go reference `otelcli/config.go` (+ `config_span.go`).
//!
//! Precedence at runtime: CLI flags > environment > JSON config file > defaults.
//! This module implements the bottom three layers; CLI overlay lives in `cli::`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config file {path:?}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse JSON in config file {path:?}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not parse {env_var} value {value:?} as {kind}: {detail}")]
    EnvParse {
        env_var: String,
        value: String,
        kind: &'static str,
        detail: String,
    },
}

#[derive(Debug, Error)]
pub enum TimeParseError {
    #[error("could not parse time {0:?} as any supported format")]
    Unparseable(String),
}

#[derive(Debug, Error)]
pub enum DurationParseError {
    #[error("unable to parse duration string {0:?}")]
    Unparseable(String),
}

/// Resolved transport for a configured endpoint. The Go code computes this
/// lazily inside `StartClient`; we hoist it to an explicit type so the client
/// factory has a single switch to match on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    /// gRPC over HTTP/2.
    Grpc,
    /// OTLP/HTTP with `application/x-protobuf`.
    HttpProto,
    /// OTLP/HTTP with `application/json` (otel-cli extension).
    HttpJson,
    /// Write spans directly to a directory tree, matching `server json` layout
    /// (otel-cli extension). Used when `protocol == "json+file"`.
    JsonFile(PathBuf),
    /// No endpoint configured — non-recording mode.
    Null,
}

/// Tee fanout mode (PRD §9.1). Selected separately from [`Protocol`] so a single
/// run can record to both the network OTLP receiver and a durable directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeeMode {
    /// No tee — single transport (existing default behaviour).
    Off,
    /// Fan out to the configured network OTLP transport *and* a json+file sink.
    OtlpAndFile,
    /// Network OTLP only (still flagged so callers know tee was opted into).
    OtlpOnly,
    /// File only (no network export).
    FileOnly,
}

/// Runtime configuration. Field names mirror Go's `Config` struct verbatim
/// (snake_case for JSON keys) so JSON config files are wire-compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub traces_endpoint: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub timeout: String,
    #[serde(default, rename = "otlp_headers")]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub insecure: bool,
    #[serde(default, rename = "otlp_blocking")]
    pub blocking: bool,

    #[serde(default)]
    pub tls_ca_cert: String,
    #[serde(default)]
    pub tls_client_key: String,
    #[serde(default)]
    pub tls_client_cert: String,
    #[serde(default)]
    pub tls_no_verify: bool,

    #[serde(default)]
    pub service_name: String,
    #[serde(default)]
    pub span_name: String,
    #[serde(default, rename = "span_kind")]
    pub kind: String,
    #[serde(default, rename = "span_attributes")]
    pub attributes: BTreeMap<String, String>,
    /// Process-wide attributes attached to the OTel Resource (not the span).
    /// Loaded from OTEL_RESOURCE_ATTRIBUTES at env-overlay time. Every emitted
    /// `ResourceSpans` carries these so callers don't need to repeat them on
    /// every span.
    #[serde(default)]
    pub resource_attributes: BTreeMap<String, String>,
    #[serde(default, rename = "span_status_code")]
    pub status_code: String,
    #[serde(default, rename = "span_status_description")]
    pub status_description: String,

    #[serde(default)]
    pub force_span_id: String,
    #[serde(default)]
    pub force_parent_span_id: String,
    #[serde(default)]
    pub force_trace_id: String,

    #[serde(default)]
    pub traceparent_carrier_file: String,
    #[serde(default)]
    pub traceparent_ignore_env: bool,
    #[serde(default)]
    pub traceparent_print: bool,
    #[serde(default)]
    pub traceparent_print_export: bool,
    #[serde(default)]
    pub traceparent_required: bool,

    #[serde(default)]
    pub background_parent_poll_ms: u64,
    #[serde(default, rename = "background_socket_directory")]
    pub background_sockdir: String,
    #[serde(default)]
    pub background_wait: bool,
    #[serde(default)]
    pub background_skip_parent_pid_check: bool,

    #[serde(default)]
    pub exec_command_timeout: String,
    #[serde(default)]
    pub exec_tp_disable_inject: bool,

    #[serde(default)]
    pub status_canary_count: u32,
    #[serde(default)]
    pub status_canary_interval: String,

    #[serde(default)]
    pub span_start_time: String,
    #[serde(default)]
    pub span_end_time: String,
    #[serde(default)]
    pub event_name: String,
    #[serde(default)]
    pub event_time: String,

    #[serde(default, rename = "config_file")]
    pub cfg_file: String,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub fail: bool,
    /// Not serialized — passed in from main() for diagnostics output.
    #[serde(skip)]
    pub version: String,

    /// otel-cli extension: directory used when `protocol == "json+file"`.
    /// Not present in Go.
    #[serde(default)]
    pub json_dir: String,

    /// Tee-fanout selector. One of `""` (off), `"otlp+file"`, `"otlp-only"`,
    /// `"file-only"`. Driven by `OTEL_CLI_TEE` or `--tee`. PRD §9.1.
    #[serde(default)]
    pub tee: String,

    /// Directory for the file leg of the tee (PRD §9.1). When empty and
    /// [`Self::tee`] selects a file leg, [`Self::json_dir`] is used instead.
    /// Driven by `OTEL_CLI_TEE_FILE_DIR` / `--tee-file-dir`.
    #[serde(default)]
    pub tee_file_dir: String,

    /// Request fdatasync + parent-directory fsync on every span/event write.
    /// Driven by `OTEL_CLI_TEE_DURABLE=1` / `--tee-durable`. PRD §9.1.
    #[serde(default)]
    pub tee_durable: bool,

    /// Directory for the crash-durable span checkpoint log (PRD §9.3). When
    /// empty, checkpointing is disabled (no behaviour change for non-CI use).
    /// Driven by `OTEL_CHECKPOINT_DIR`.
    #[serde(default)]
    pub checkpoint_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Config {
    /// Mirror Go's `DefaultConfig()` in `config.go:29-72`.
    pub fn defaults() -> Self {
        Self {
            endpoint: String::new(),
            traces_endpoint: String::new(),
            protocol: String::new(),
            timeout: "1s".to_string(),
            headers: BTreeMap::new(),
            insecure: false,
            blocking: false,

            tls_ca_cert: String::new(),
            tls_client_key: String::new(),
            tls_client_cert: String::new(),
            tls_no_verify: false,

            service_name: "otel-cli".to_string(),
            span_name: "todo-generate-default-span-names".to_string(),
            kind: "client".to_string(),
            attributes: BTreeMap::new(),
            resource_attributes: BTreeMap::new(),
            status_code: "unset".to_string(),
            status_description: String::new(),

            force_span_id: String::new(),
            force_parent_span_id: String::new(),
            force_trace_id: String::new(),

            traceparent_carrier_file: String::new(),
            traceparent_ignore_env: false,
            traceparent_print: false,
            traceparent_print_export: false,
            traceparent_required: false,

            background_parent_poll_ms: 10,
            background_sockdir: String::new(),
            background_wait: false,
            background_skip_parent_pid_check: false,

            exec_command_timeout: String::new(),
            exec_tp_disable_inject: false,

            status_canary_count: 1,
            status_canary_interval: String::new(),

            span_start_time: "now".to_string(),
            span_end_time: "now".to_string(),
            event_name: "todo-generate-default-event-names".to_string(),
            event_time: "now".to_string(),

            cfg_file: String::new(),
            verbose: false,
            fail: false,
            version: "unset".to_string(),

            json_dir: String::new(),

            tee: String::new(),
            tee_file_dir: String::new(),
            tee_durable: false,
            checkpoint_dir: String::new(),
        }
    }

    /// Load a JSON config file in place. Returns `Ok(())` if the path is
    /// non-empty and parses cleanly. Mirrors Go's `Config.LoadFile`.
    pub fn load_file<P: AsRef<Path>>(&mut self, path: P) -> Result<(), ConfigError> {
        let path_ref = path.as_ref();
        let display = path_ref.display().to_string();
        let bytes = fs::read(path_ref).map_err(|source| ConfigError::Read {
            path: display.clone(),
            source,
        })?;
        let parsed: Config = serde_json::from_slice(&bytes).map_err(|source| ConfigError::Json {
            path: display.clone(),
            source,
        })?;
        *self = parsed;
        self.cfg_file = display;
        Ok(())
    }

    /// Walk the env-var mapping (table below) and overwrite any field for
    /// which the corresponding env var is non-empty. For fields with multiple
    /// candidate env vars, the first non-empty wins. Mirrors Go's
    /// `LoadEnv(os.Getenv)`.
    pub fn load_env(&mut self) -> Result<(), ConfigError> {
        self.load_env_with(|name| std::env::var(name))
    }

    /// Test-friendly variant: lookup is delegated to a closure.
    pub fn load_env_with<F>(&mut self, getenv: F) -> Result<(), ConfigError>
    where
        F: Fn(&str) -> Result<String, std::env::VarError>,
    {
        // Helper to fetch the first non-empty value across a list of names.
        let first = |names: &[&str]| -> Option<(String, String)> {
            for name in names {
                match getenv(name) {
                    Ok(v) if !v.is_empty() => return Some((name.to_string(), v)),
                    _ => continue,
                }
            }
            None
        };

        macro_rules! str_field {
            ($field:ident, $names:expr) => {
                if let Some((_, v)) = first(&$names) {
                    self.$field = v;
                }
            };
        }

        macro_rules! bool_field {
            ($field:ident, $names:expr) => {
                if let Some((env_var, v)) = first(&$names) {
                    self.$field = parse_bool(&v).ok_or_else(|| ConfigError::EnvParse {
                        env_var,
                        value: v.clone(),
                        kind: "bool",
                        detail: "expected true/false/1/0".to_string(),
                    })?;
                }
            };
        }

        macro_rules! map_field {
            ($field:ident, $names:expr) => {
                if let Some((env_var, v)) = first(&$names) {
                    self.$field = parse_attrs(&v).map_err(|detail| ConfigError::EnvParse {
                        env_var,
                        value: v.clone(),
                        kind: "map",
                        detail,
                    })?;
                }
            };
        }

        str_field!(endpoint, ["OTEL_EXPORTER_OTLP_ENDPOINT"]);
        str_field!(traces_endpoint, ["OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"]);
        str_field!(
            protocol,
            ["OTEL_EXPORTER_OTLP_PROTOCOL", "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL"]
        );
        str_field!(
            timeout,
            ["OTEL_EXPORTER_OTLP_TIMEOUT", "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT"]
        );
        map_field!(headers, ["OTEL_EXPORTER_OTLP_HEADERS"]);
        bool_field!(insecure, ["OTEL_EXPORTER_OTLP_INSECURE"]);
        bool_field!(blocking, ["OTEL_EXPORTER_OTLP_BLOCKING"]);

        str_field!(
            tls_ca_cert,
            [
                "OTEL_EXPORTER_OTLP_CERTIFICATE",
                "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE"
            ]
        );
        str_field!(
            tls_client_key,
            [
                "OTEL_EXPORTER_OTLP_CLIENT_KEY",
                "OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY"
            ]
        );
        str_field!(
            tls_client_cert,
            [
                "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
                "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE"
            ]
        );
        bool_field!(
            tls_no_verify,
            ["OTEL_CLI_TLS_NO_VERIFY", "OTEL_CLI_NO_TLS_VERIFY"]
        );

        str_field!(service_name, ["OTEL_CLI_SERVICE_NAME", "OTEL_SERVICE_NAME"]);
        str_field!(span_name, ["OTEL_CLI_SPAN_NAME"]);
        str_field!(kind, ["OTEL_CLI_TRACE_KIND"]);
        map_field!(attributes, ["OTEL_CLI_ATTRIBUTES"]);
        // Process-wide resource attributes. The standard OTel env var is
        // OTEL_RESOURCE_ATTRIBUTES; same comma-separated k=v format as
        // OTEL_CLI_ATTRIBUTES. Applied to every emitted ResourceSpans.
        map_field!(resource_attributes, ["OTEL_RESOURCE_ATTRIBUTES"]);
        str_field!(status_code, ["OTEL_CLI_STATUS_CODE"]);
        str_field!(status_description, ["OTEL_CLI_STATUS_DESCRIPTION"]);
        str_field!(force_span_id, ["OTEL_CLI_FORCE_SPAN_ID"]);
        str_field!(force_parent_span_id, ["OTEL_CLI_FORCE_PARENT_SPAN_ID"]);
        str_field!(force_trace_id, ["OTEL_CLI_FORCE_TRACE_ID"]);

        str_field!(traceparent_carrier_file, ["OTEL_CLI_CARRIER_FILE"]);
        bool_field!(traceparent_ignore_env, ["OTEL_CLI_IGNORE_ENV"]);
        bool_field!(traceparent_print, ["OTEL_CLI_PRINT_TRACEPARENT"]);
        bool_field!(traceparent_print_export, ["OTEL_CLI_EXPORT_TRACEPARENT"]);
        bool_field!(traceparent_required, ["OTEL_CLI_TRACEPARENT_REQUIRED"]);

        str_field!(exec_command_timeout, ["OTEL_CLI_EXEC_CMD_TIMEOUT"]);
        bool_field!(exec_tp_disable_inject, ["OTEL_CLI_EXEC_TP_DISABLE_INJECT"]);

        str_field!(cfg_file, ["OTEL_CLI_CONFIG_FILE"]);
        bool_field!(verbose, ["OTEL_CLI_VERBOSE"]);
        bool_field!(fail, ["OTEL_CLI_FAIL"]);

        // PRD §9.1 tee fanout.
        str_field!(tee, ["OTEL_CLI_TEE"]);
        str_field!(tee_file_dir, ["OTEL_CLI_TEE_FILE_DIR"]);
        bool_field!(tee_durable, ["OTEL_CLI_TEE_DURABLE"]);

        // PRD §9.3 crash-durable checkpoint log.
        str_field!(checkpoint_dir, ["OTEL_CHECKPOINT_DIR"]);

        Ok(())
    }

    /// Merge `other` into `self`, only overriding fields in `self` that still
    /// hold their default value. Mirrors the Go behaviour where env > file
    /// merges layer-by-layer (envs only fill what file didn't set, etc.).
    pub fn merge_from(&mut self, other: &Config) {
        let d = Self::defaults();
        if self.endpoint == d.endpoint && !other.endpoint.is_empty() {
            self.endpoint = other.endpoint.clone();
        }
        if self.traces_endpoint == d.traces_endpoint && !other.traces_endpoint.is_empty() {
            self.traces_endpoint = other.traces_endpoint.clone();
        }
        if self.protocol == d.protocol && !other.protocol.is_empty() {
            self.protocol = other.protocol.clone();
        }
        if self.timeout == d.timeout && !other.timeout.is_empty() {
            self.timeout = other.timeout.clone();
        }
        if self.headers == d.headers && !other.headers.is_empty() {
            self.headers = other.headers.clone();
        }
        if self.insecure == d.insecure && other.insecure {
            self.insecure = other.insecure;
        }
        if self.blocking == d.blocking && other.blocking {
            self.blocking = other.blocking;
        }

        if self.tls_ca_cert == d.tls_ca_cert && !other.tls_ca_cert.is_empty() {
            self.tls_ca_cert = other.tls_ca_cert.clone();
        }
        if self.tls_client_key == d.tls_client_key && !other.tls_client_key.is_empty() {
            self.tls_client_key = other.tls_client_key.clone();
        }
        if self.tls_client_cert == d.tls_client_cert && !other.tls_client_cert.is_empty() {
            self.tls_client_cert = other.tls_client_cert.clone();
        }
        if self.tls_no_verify == d.tls_no_verify && other.tls_no_verify {
            self.tls_no_verify = other.tls_no_verify;
        }

        if self.service_name == d.service_name && !other.service_name.is_empty() {
            self.service_name = other.service_name.clone();
        }
        if self.span_name == d.span_name && !other.span_name.is_empty() {
            self.span_name = other.span_name.clone();
        }
        if self.kind == d.kind && !other.kind.is_empty() {
            self.kind = other.kind.clone();
        }
        if self.attributes == d.attributes && !other.attributes.is_empty() {
            self.attributes = other.attributes.clone();
        }
        if self.status_code == d.status_code && !other.status_code.is_empty() {
            self.status_code = other.status_code.clone();
        }
        if self.status_description == d.status_description && !other.status_description.is_empty()
        {
            self.status_description = other.status_description.clone();
        }
        if self.force_span_id == d.force_span_id && !other.force_span_id.is_empty() {
            self.force_span_id = other.force_span_id.clone();
        }
        if self.force_parent_span_id == d.force_parent_span_id
            && !other.force_parent_span_id.is_empty()
        {
            self.force_parent_span_id = other.force_parent_span_id.clone();
        }
        if self.force_trace_id == d.force_trace_id && !other.force_trace_id.is_empty() {
            self.force_trace_id = other.force_trace_id.clone();
        }

        if self.traceparent_carrier_file == d.traceparent_carrier_file
            && !other.traceparent_carrier_file.is_empty()
        {
            self.traceparent_carrier_file = other.traceparent_carrier_file.clone();
        }
        if self.traceparent_ignore_env == d.traceparent_ignore_env && other.traceparent_ignore_env
        {
            self.traceparent_ignore_env = other.traceparent_ignore_env;
        }
        if self.traceparent_print == d.traceparent_print && other.traceparent_print {
            self.traceparent_print = other.traceparent_print;
        }
        if self.traceparent_print_export == d.traceparent_print_export
            && other.traceparent_print_export
        {
            self.traceparent_print_export = other.traceparent_print_export;
        }
        if self.traceparent_required == d.traceparent_required && other.traceparent_required {
            self.traceparent_required = other.traceparent_required;
        }

        if self.background_parent_poll_ms == d.background_parent_poll_ms
            && other.background_parent_poll_ms != d.background_parent_poll_ms
        {
            self.background_parent_poll_ms = other.background_parent_poll_ms;
        }
        if self.background_sockdir == d.background_sockdir && !other.background_sockdir.is_empty()
        {
            self.background_sockdir = other.background_sockdir.clone();
        }
        if self.background_wait == d.background_wait && other.background_wait {
            self.background_wait = other.background_wait;
        }
        if self.background_skip_parent_pid_check == d.background_skip_parent_pid_check
            && other.background_skip_parent_pid_check
        {
            self.background_skip_parent_pid_check = other.background_skip_parent_pid_check;
        }

        if self.exec_command_timeout == d.exec_command_timeout
            && !other.exec_command_timeout.is_empty()
        {
            self.exec_command_timeout = other.exec_command_timeout.clone();
        }
        if self.exec_tp_disable_inject == d.exec_tp_disable_inject && other.exec_tp_disable_inject
        {
            self.exec_tp_disable_inject = other.exec_tp_disable_inject;
        }

        if self.status_canary_count == d.status_canary_count
            && other.status_canary_count != d.status_canary_count
        {
            self.status_canary_count = other.status_canary_count;
        }
        if self.status_canary_interval == d.status_canary_interval
            && !other.status_canary_interval.is_empty()
        {
            self.status_canary_interval = other.status_canary_interval.clone();
        }

        if self.span_start_time == d.span_start_time && !other.span_start_time.is_empty() {
            self.span_start_time = other.span_start_time.clone();
        }
        if self.span_end_time == d.span_end_time && !other.span_end_time.is_empty() {
            self.span_end_time = other.span_end_time.clone();
        }
        if self.event_name == d.event_name && !other.event_name.is_empty() {
            self.event_name = other.event_name.clone();
        }
        if self.event_time == d.event_time && !other.event_time.is_empty() {
            self.event_time = other.event_time.clone();
        }

        if self.cfg_file == d.cfg_file && !other.cfg_file.is_empty() {
            self.cfg_file = other.cfg_file.clone();
        }
        if self.verbose == d.verbose && other.verbose {
            self.verbose = other.verbose;
        }
        if self.fail == d.fail && other.fail {
            self.fail = other.fail;
        }
        if self.json_dir == d.json_dir && !other.json_dir.is_empty() {
            self.json_dir = other.json_dir.clone();
        }

        if self.tee == d.tee && !other.tee.is_empty() {
            self.tee = other.tee.clone();
        }
        if self.tee_file_dir == d.tee_file_dir && !other.tee_file_dir.is_empty() {
            self.tee_file_dir = other.tee_file_dir.clone();
        }
        if self.tee_durable == d.tee_durable && other.tee_durable {
            self.tee_durable = other.tee_durable;
        }
        if self.checkpoint_dir == d.checkpoint_dir && !other.checkpoint_dir.is_empty() {
            self.checkpoint_dir = other.checkpoint_dir.clone();
        }
    }

    /// True when an endpoint or directly-attached recording output is set.
    /// Mirrors Go's `GetIsRecording`, plus the otel-cli-rs `json+file` path
    /// and the PRD §9.1 tee modes.
    pub fn is_recording(&self) -> bool {
        !self.endpoint.is_empty()
            || !self.traces_endpoint.is_empty()
            || self.protocol == "json+file"
            || !self.json_dir.is_empty()
            || self.resolved_tee_mode() != TeeMode::Off
    }

    /// Compute the wire protocol from `protocol` + endpoint scheme. Returns
    /// [`Protocol::Null`] when not recording.
    pub fn resolved_protocol(&self) -> Protocol {
        if !self.is_recording() {
            return Protocol::Null;
        }

        // explicit otel-cli-rs extension: write spans directly to disk
        if self.protocol == "json+file" || !self.json_dir.is_empty() {
            let dir = if !self.json_dir.is_empty() {
                PathBuf::from(&self.json_dir)
            } else {
                PathBuf::from(".")
            };
            return Protocol::JsonFile(dir);
        }

        match self.protocol.as_str() {
            "grpc" => return Protocol::Grpc,
            "http/protobuf" => return Protocol::HttpProto,
            "http/json" => return Protocol::HttpJson,
            _ => {}
        }

        // Fall back on endpoint scheme. Signal-specific endpoint takes
        // precedence per OTel spec.
        let endpoint = if !self.traces_endpoint.is_empty() {
            self.traces_endpoint.as_str()
        } else {
            self.endpoint.as_str()
        };

        if let Some(scheme) = endpoint.split("://").next().filter(|s| *s != endpoint) {
            match scheme {
                "http" | "https" => Protocol::HttpProto,
                "grpc" => Protocol::Grpc,
                _ => Protocol::Grpc,
            }
        } else {
            // bare host:port: gRPC per Go's `ParseEndpoint`
            Protocol::Grpc
        }
    }

    /// Parse the configured timeout into a [`Duration`].
    pub fn parse_timeout(&self) -> Result<Duration, DurationParseError> {
        parse_duration(&self.timeout)
    }

    /// Resolve the tee fanout mode (PRD §9.1). Selected independently from
    /// [`Self::resolved_protocol`]; the client factory composes the two.
    pub fn resolved_tee_mode(&self) -> TeeMode {
        match self.tee.as_str() {
            "" | "off" => TeeMode::Off,
            "otlp+file" | "both" => TeeMode::OtlpAndFile,
            "otlp-only" | "otlp" => TeeMode::OtlpOnly,
            "file-only" | "file" => TeeMode::FileOnly,
            // Unknown value: treat as off rather than failing — the surface area
            // is small enough that strict validation isn't worth a UX hit.
            _ => TeeMode::Off,
        }
    }

    /// Resolve the checkpoint directory (PRD §9.3). Returns `None` when
    /// unset — checkpointing is opt-in via `OTEL_CHECKPOINT_DIR` so non-CI
    /// uses pay nothing.
    pub fn resolved_checkpoint_dir(&self) -> Option<PathBuf> {
        if self.checkpoint_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.checkpoint_dir))
        }
    }

    /// Resolve the directory used for the file leg of the tee. Falls back to
    /// [`Self::json_dir`] when [`Self::tee_file_dir`] is empty.
    pub fn resolved_tee_file_dir(&self) -> PathBuf {
        if !self.tee_file_dir.is_empty() {
            PathBuf::from(&self.tee_file_dir)
        } else if !self.json_dir.is_empty() {
            PathBuf::from(&self.json_dir)
        } else {
            PathBuf::from(".")
        }
    }
}

/// Parse the broad set of CLI-friendly time strings supported by otel-cli.
///
/// Accepted forms (in order of attempt):
/// - `"now"` → current UTC
/// - Unix epoch seconds, e.g. `"1616620946"`
/// - Unix epoch seconds with fractional nanoseconds, e.g. `"1616620946.241980634"`
/// - RFC3339 / RFC3339Nano, e.g. `"2021-03-24T07:28:05Z"`
/// - RFC3339 with a space instead of `T`, e.g. `"2021-03-24 07:28:05Z"`
///   (the `date --rfc-3339` output format)
pub fn parse_cli_time(ts: &str) -> Result<DateTime<Utc>, TimeParseError> {
    if ts == "now" {
        return Ok(Utc::now());
    }

    // Unix epoch (whole seconds) — try this before RFC3339 because plain
    // integers wouldn't match any RFC3339 form.
    if let Ok(i) = ts.parse::<i64>() {
        if let Some(dt) = Utc.timestamp_opt(i, 0).single() {
            return Ok(dt);
        }
    }

    // Fix the `date --rfc-3339` form (space instead of T).
    let fixed = if broken_rfc3339_re().is_match(ts) {
        ts.replacen(' ', "T", 1)
    } else {
        ts.to_string()
    };

    // Unix epoch seconds + fractional nanos: e.g. "1616620946.241980634"
    if epoch_nano_re().is_match(&fixed) {
        if let Some((secs_part, frac_part)) = fixed.split_once('.') {
            if let (Ok(secs), Ok(nsecs)) = (secs_part.parse::<i64>(), frac_part.parse::<u32>()) {
                if secs > 0 {
                    if let Some(dt) = Utc.timestamp_opt(secs, nsecs).single() {
                        return Ok(dt);
                    }
                }
            }
        }
    }

    // RFC3339 (also accepts the nanosecond-bearing form).
    if let Ok(dt) = DateTime::parse_from_rfc3339(&fixed) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Some inputs may not carry a timezone — try naive then assume UTC.
    if let Ok(naive) = NaiveDateTime::parse_from_str(&fixed, "%Y-%m-%dT%H:%M:%S%.f") {
        return Ok(Utc.from_utc_datetime(&naive));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(&fixed, "%Y-%m-%dT%H:%M:%S") {
        return Ok(Utc.from_utc_datetime(&naive));
    }

    Err(TimeParseError::Unparseable(ts.to_string()))
}

/// Parse a `k1=v1,k2=v2` attribute string. Values may contain `=`, but `,`
/// separates pairs (matching Go's CSV-like parser).
///
/// Empty input yields an empty map; malformed pairs (missing `=`) yield an
/// `Err`. The result type is `BTreeMap` to preserve deterministic ordering.
pub fn parse_attrs(s: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    if s.is_empty() {
        return Ok(out);
    }
    for pair in s.split(',') {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("kv pair {pair} must be in key=value format"))?;
        if k.is_empty() || v.is_empty() {
            return Err(format!("kv pair {pair} must be in key=value format"));
        }
        out.insert(k.to_string(), v.to_string());
    }
    Ok(out)
}

/// Alias of [`parse_attrs`]; same wire format.
pub fn parse_headers(s: &str) -> Result<BTreeMap<String, String>, String> {
    parse_attrs(s)
}

/// Parse a Go-style duration: "1s", "500ms", "1h30m", "2us", "5µs".
///
/// `humantime` doesn't accept the bare `us` or `µs` microseconds spellings, so
/// we map them to its `usec`/`us` form. Bare integers are interpreted as seconds
/// (matching Go's `parseDuration`).
pub fn parse_duration(s: &str) -> Result<Duration, DurationParseError> {
    if s.is_empty() {
        return Ok(Duration::ZERO);
    }

    // Try humantime first.
    if let Ok(d) = humantime::parse_duration(s) {
        return Ok(d);
    }

    // Fix-up: Go accepts "µs" and "us" for microseconds. humantime accepts both,
    // so this is largely a defensive path for unusual locales.
    let fixed = s.replace('µ', "u");
    if let Ok(d) = humantime::parse_duration(&fixed) {
        return Ok(d);
    }

    // Try as bare seconds (Go falls through to this case for unsuffixed inputs).
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }

    Err(DurationParseError::Unparseable(s.to_string()))
}

fn broken_rfc3339_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{4}-\d{2}-\d{2} ").expect("broken-rfc3339 regex compiles"))
}

fn epoch_nano_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\.\d+$").expect("epoch-nano regex compiles"))
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "1" | "t" | "true" | "yes" | "on" => Some(true),
        "0" | "f" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that mutate process-wide environment so they don't race
    // with parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_config_matches_go() {
        let c = Config::defaults();
        assert_eq!(c.service_name, "otel-cli");
        assert_eq!(c.kind, "client");
        assert_eq!(c.timeout, "1s");
        assert_eq!(c.span_name, "todo-generate-default-span-names");
        assert_eq!(c.event_name, "todo-generate-default-event-names");
        assert_eq!(c.span_start_time, "now");
        assert_eq!(c.span_end_time, "now");
        assert_eq!(c.event_time, "now");
        assert_eq!(c.status_code, "unset");
        assert_eq!(c.version, "unset");
        assert_eq!(c.background_parent_poll_ms, 10);
        assert_eq!(c.status_canary_count, 1);
        assert!(!c.insecure);
        assert!(!c.blocking);
        assert!(!c.tls_no_verify);
        assert!(c.headers.is_empty());
        assert!(c.attributes.is_empty());
    }

    #[test]
    fn parse_cli_time_now() {
        let before = Utc::now();
        let t = parse_cli_time("now").unwrap();
        let after = Utc::now();
        assert!(t >= before && t <= after);
    }

    #[test]
    fn parse_cli_time_rfc3339() {
        let t = parse_cli_time("2021-03-24T07:28:05.12345Z").unwrap();
        assert_eq!(t.format("%Y-%m-%dT%H:%M:%S").to_string(), "2021-03-24T07:28:05");
    }

    #[test]
    fn parse_cli_time_unix_epoch() {
        let t = parse_cli_time("1616620946").unwrap();
        assert_eq!(t.timestamp(), 1616620946);
    }

    #[test]
    fn parse_cli_time_unix_epoch_nanos() {
        let t = parse_cli_time("1616620946.241980634").unwrap();
        assert_eq!(t.timestamp(), 1616620946);
        assert_eq!(t.timestamp_subsec_nanos(), 241980634);
    }

    #[test]
    fn parse_cli_time_broken_rfc3339_prefix() {
        // Go's "date --rfc-3339=ns" produces "2021-03-24 07:28:05.123456789+00:00"
        // — space instead of T. Reuse a UTC-Z variant to keep the test simple.
        let t = parse_cli_time("2021-03-24 07:28:05Z").unwrap();
        assert_eq!(t.timestamp(), 1616570885);
    }

    #[test]
    fn parse_cli_time_invalid_errs() {
        let err = parse_cli_time("not a date");
        assert!(err.is_err());
    }

    #[test]
    fn parse_attrs_basic() {
        let m = parse_attrs("a=1,b=2").unwrap();
        assert_eq!(m.get("a"), Some(&"1".to_string()));
        assert_eq!(m.get("b"), Some(&"2".to_string()));
    }

    #[test]
    fn parse_attrs_with_equals_in_value() {
        let m = parse_attrs("a=key=val,b=2").unwrap();
        assert_eq!(m.get("a"), Some(&"key=val".to_string()));
        assert_eq!(m.get("b"), Some(&"2".to_string()));
    }

    #[test]
    fn parse_attrs_empty_input() {
        let m = parse_attrs("").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn parse_attrs_malformed_errs() {
        let err = parse_attrs("no equals here");
        assert!(err.is_err());
    }

    #[test]
    fn parse_headers_basic() {
        let m = parse_headers("Authorization=Bearer abc,X-Trace=1").unwrap();
        assert_eq!(m.get("Authorization"), Some(&"Bearer abc".to_string()));
        assert_eq!(m.get("X-Trace"), Some(&"1".to_string()));
    }

    #[test]
    fn parse_duration_seconds_suffix() {
        assert_eq!(parse_duration("1s").unwrap(), Duration::from_secs(1));
    }

    #[test]
    fn parse_duration_millis_suffix() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn parse_duration_bare_int_is_seconds() {
        assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn parse_duration_empty_is_zero() {
        assert_eq!(parse_duration("").unwrap(), Duration::ZERO);
    }

    #[test]
    fn load_env_simple() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg = Config::defaults();
        let getenv = |k: &str| {
            if k == "OTEL_EXPORTER_OTLP_ENDPOINT" {
                Ok("http://localhost:4318".to_string())
            } else {
                Err(std::env::VarError::NotPresent)
            }
        };
        cfg.load_env_with(getenv).unwrap();
        assert_eq!(cfg.endpoint, "http://localhost:4318");
    }

    #[test]
    fn load_env_multi_fallback() {
        let _guard = ENV_LOCK.lock().unwrap();

        // First env-var unset, second set: should take the second.
        let mut cfg = Config::defaults();
        let getenv = |k: &str| {
            if k == "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL" {
                Ok("http/protobuf".to_string())
            } else {
                Err(std::env::VarError::NotPresent)
            }
        };
        cfg.load_env_with(getenv).unwrap();
        assert_eq!(cfg.protocol, "http/protobuf");

        // Both set: first wins.
        let mut cfg = Config::defaults();
        let getenv = |k: &str| match k {
            "OTEL_EXPORTER_OTLP_PROTOCOL" => Ok("grpc".to_string()),
            "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL" => Ok("http/protobuf".to_string()),
            _ => Err(std::env::VarError::NotPresent),
        };
        cfg.load_env_with(getenv).unwrap();
        assert_eq!(cfg.protocol, "grpc");
    }

    #[test]
    fn load_env_bool_and_map() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg = Config::defaults();
        let getenv = |k: &str| match k {
            "OTEL_EXPORTER_OTLP_INSECURE" => Ok("true".to_string()),
            "OTEL_EXPORTER_OTLP_HEADERS" => Ok("a=1,b=2".to_string()),
            _ => Err(std::env::VarError::NotPresent),
        };
        cfg.load_env_with(getenv).unwrap();
        assert!(cfg.insecure);
        assert_eq!(cfg.headers.get("a"), Some(&"1".to_string()));
    }

    #[test]
    fn merge_from_only_overrides_defaults() {
        // self has non-default service_name, other tries to override → self wins
        let mut a = Config::defaults();
        a.service_name = "my-service".to_string();

        let mut b = Config::defaults();
        b.service_name = "other-service".to_string();
        b.endpoint = "http://localhost:4317".to_string();

        a.merge_from(&b);

        // existing non-default value is kept
        assert_eq!(a.service_name, "my-service");
        // default value gets overridden by other's set value
        assert_eq!(a.endpoint, "http://localhost:4317");
    }

    #[test]
    fn is_recording_with_endpoint() {
        let mut cfg = Config::defaults();
        assert!(!cfg.is_recording());
        cfg.endpoint = "http://example".to_string();
        assert!(cfg.is_recording());
    }

    #[test]
    fn is_recording_with_json_file() {
        let mut cfg = Config::defaults();
        cfg.protocol = "json+file".to_string();
        cfg.json_dir = "/tmp/x".to_string();
        assert!(cfg.is_recording());
    }

    #[test]
    fn resolved_protocol_null_when_no_endpoint() {
        let cfg = Config::defaults();
        assert_eq!(cfg.resolved_protocol(), Protocol::Null);
    }

    #[test]
    fn resolved_protocol_explicit_grpc() {
        let mut cfg = Config::defaults();
        cfg.endpoint = "localhost:4317".to_string();
        cfg.protocol = "grpc".to_string();
        assert_eq!(cfg.resolved_protocol(), Protocol::Grpc);
    }

    #[test]
    fn resolved_protocol_http_proto_explicit() {
        let mut cfg = Config::defaults();
        cfg.endpoint = "http://localhost:4318".to_string();
        cfg.protocol = "http/protobuf".to_string();
        assert_eq!(cfg.resolved_protocol(), Protocol::HttpProto);
    }

    #[test]
    fn resolved_protocol_http_from_url() {
        let mut cfg = Config::defaults();
        cfg.endpoint = "http://localhost:4318".to_string();
        // no explicit protocol → infer from scheme
        assert_eq!(cfg.resolved_protocol(), Protocol::HttpProto);
    }

    #[test]
    fn resolved_protocol_json_file() {
        let mut cfg = Config::defaults();
        cfg.protocol = "json+file".to_string();
        cfg.json_dir = "/tmp/output".to_string();
        let p = cfg.resolved_protocol();
        match p {
            Protocol::JsonFile(dir) => assert_eq!(dir, PathBuf::from("/tmp/output")),
            other => panic!("expected JsonFile, got {other:?}"),
        }
    }

    #[test]
    fn load_file_reads_json() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"{"endpoint":"http://example:4317","service_name":"from-file"}"#,
        )
        .unwrap();

        let mut cfg = Config::defaults();
        cfg.load_file(tmp.path()).unwrap();
        assert_eq!(cfg.endpoint, "http://example:4317");
        assert_eq!(cfg.service_name, "from-file");
    }
}
