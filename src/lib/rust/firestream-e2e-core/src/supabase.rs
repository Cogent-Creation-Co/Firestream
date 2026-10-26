//! Functional probes for the Supabase stack, shared by the docker-compose
//! and the k3d harnesses.
//!
//! Each probe exercises one service end to end through the Envoy gateway, the
//! way a client does: Auth `/health`, PostgREST with the anon key, a Storage
//! upload and download, a Realtime `postgres_changes` event over a websocket
//! (once with the legacy anon key, once with the `sb_publishable_*` key), a
//! query through the Supavisor pooler, and Studio behind the dashboard
//! login.
//!
//! The harness supplies the transport: an [`Exec`] that reaches three logical
//! targets (`db`, `studio`, `gateway`), the gateway URL as the harness host
//! sees it, and the in-stack addresses of the gateway and Supavisor. The keys
//! are read from the running gateway and database (`printenv`), so the probes
//! follow whatever key set the stack was deployed with.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::exec::Exec;
use crate::probe::Probe;

/// Table the REST and Realtime probes write to.
const TABLE: &str = "firestream_e2e";
/// Storage bucket the round-trip probe uses.
const BUCKET: &str = "firestream-e2e";

/// How a harness reaches one Supabase stack.
pub struct SupabaseStack {
    /// Runs a command in a logical target: `db`, `studio` or `gateway`.
    pub exec: Arc<dyn Exec>,
    /// Exec target names for the three logical targets.
    pub db_target: String,
    pub studio_target: String,
    pub gateway_target: String,
    /// Gateway base URL as the harness host sees it (no trailing slash).
    pub gateway_url: String,
    /// Gateway base URL as the `studio` target sees it.
    pub internal_gateway_url: String,
    /// Supavisor host name as the `db` target sees it.
    pub supavisor_host: String,
}

struct Keys {
    anon: String,
    service: String,
    dashboard_user: String,
    dashboard_password: String,
    db_password: String,
}

impl SupabaseStack {
    fn printenv(&self, target: &str, name: &str) -> Result<String> {
        let out = self.exec.exec(target, &["printenv", name])?;
        if !out.status.success() {
            bail!("printenv {} in {} exited {}", name, target, out.status);
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    fn keys(&self) -> Result<Keys> {
        let gw = &self.gateway_target;
        Ok(Keys {
            anon: self.printenv(gw, "ANON_KEY")?,
            service: self.printenv(gw, "SERVICE_ROLE_KEY")?,
            // The gateway image defaults the user name when the key set omits it.
            dashboard_user: self
                .printenv(gw, "DASHBOARD_USERNAME")
                .ok()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| "supabase".into()),
            dashboard_password: self.printenv(gw, "DASHBOARD_PASSWORD")?,
            db_password: self.printenv(&self.db_target, "POSTGRESQL_PASSWORD")?,
        })
    }

    /// Run SQL as `postgres` inside the db target.
    fn psql(&self, keys: &Keys, host: &str, port: u16, user: &str, sql: &str) -> Result<String> {
        let url = format!("postgresql://{}:{}@{}:{}/postgres", user, keys.db_password, host, port);
        let out = self.exec.exec(
            &self.db_target,
            &["psql", &url, "-v", "ON_ERROR_STOP=1", "-tA", "-c", sql],
        )?;
        if !out.status.success() {
            bail!(
                "psql as {} on {}:{} failed: {}",
                user,
                host,
                port,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    fn ensure_table(&self, keys: &Keys) -> Result<()> {
        self.psql(
            keys,
            "127.0.0.1",
            5432,
            "postgres",
            &format!(
                "create table if not exists public.{t} (id bigint generated always as identity primary key, note text not null);
                 grant select on public.{t} to anon;
                 grant select, insert on public.{t} to service_role;
                 do $$ begin
                   if not exists (select 1 from pg_publication_tables where pubname = 'supabase_realtime' and tablename = '{t}') then
                     alter publication supabase_realtime add table public.{t};
                   end if;
                 end $$;
                 notify pgrst, 'reload schema';",
                t = TABLE
            ),
        )
        .map(|_| ())
    }
}

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("build reqwest blocking client")
}

fn expect_status(resp: reqwest::blocking::Response, what: &str, ok: &[u16]) -> Result<String> {
    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    if !ok.contains(&status) {
        bail!("{} returned {}: {}", what, status, body.chars().take(300).collect::<String>());
    }
    Ok(body)
}

/// Probe names, in dependency order.
pub const CHAIN: [&str; 7] = [
    AuthHealth::NAME,
    RestAnon::NAME,
    StorageRoundTrip::NAME,
    RealtimeChange::NAME,
    RealtimePublishable::NAME,
    SupavisorQuery::NAME,
    StudioDashboard::NAME,
];

/// The probe called `name` (one of [`CHAIN`]) against `stack`.
pub fn probe(name: &str, stack: Arc<SupabaseStack>) -> Option<Box<dyn Probe + Send + Sync>> {
    Some(match name {
        AuthHealth::NAME => Box::new(AuthHealth(stack)),
        RestAnon::NAME => Box::new(RestAnon(stack)),
        StorageRoundTrip::NAME => Box::new(StorageRoundTrip(stack)),
        RealtimeChange::NAME => Box::new(RealtimeChange(stack)),
        RealtimePublishable::NAME => Box::new(RealtimePublishable(stack)),
        SupavisorQuery::NAME => Box::new(SupavisorQuery(stack)),
        StudioDashboard::NAME => Box::new(StudioDashboard(stack)),
        _ => return None,
    })
}

/// The whole chain against `stack`, in dependency order.
pub fn probes(stack: Arc<SupabaseStack>) -> Vec<Box<dyn Probe + Send + Sync>> {
    CHAIN.iter().filter_map(|n| probe(n, stack.clone())).collect()
}

/// `GET /auth/v1/health` with the anon key.
pub struct AuthHealth(pub Arc<SupabaseStack>);

impl AuthHealth {
    pub const NAME: &'static str = "supabase_auth_health";
}

impl Probe for AuthHealth {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn run(&self) -> Result<()> {
        let keys = self.0.keys()?;
        let resp = client()?
            .get(format!("{}/auth/v1/health", self.0.gateway_url))
            .header("apikey", &keys.anon)
            .send()?;
        expect_status(resp, "auth health", &[200]).map(|_| ())
    }
}

/// PostgREST reads a table with the anon key.
pub struct RestAnon(pub Arc<SupabaseStack>);

impl RestAnon {
    pub const NAME: &'static str = "supabase_rest_anon";
}

impl Probe for RestAnon {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn run(&self) -> Result<()> {
        let keys = self.0.keys()?;
        self.0.ensure_table(&keys)?;
        let resp = client()?
            .get(format!("{}/rest/v1/{}?select=id", self.0.gateway_url, TABLE))
            .header("apikey", &keys.anon)
            .send()?;
        let body = expect_status(resp, "rest select as anon", &[200])?;
        if !body.trim_start().starts_with('[') {
            bail!("rest select returned no JSON array: {}", body);
        }
        Ok(())
    }
}

/// Storage: create a bucket, upload an object, download it, compare.
pub struct StorageRoundTrip(pub Arc<SupabaseStack>);

impl StorageRoundTrip {
    pub const NAME: &'static str = "supabase_storage_roundtrip";
}

impl Probe for StorageRoundTrip {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn run(&self) -> Result<()> {
        let keys = self.0.keys()?;
        let c = client()?;
        let gw = &self.0.gateway_url;
        let bearer = format!("Bearer {}", keys.service);
        let resp = c
            .post(format!("{}/storage/v1/bucket", gw))
            .header("apikey", &keys.service)
            .header("Authorization", &bearer)
            .header("Content-Type", "application/json")
            .body(format!(r#"{{"name":"{}","public":false}}"#, BUCKET))
            .send()?;
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        if status != 200 && !body.contains("already exists") {
            bail!("create bucket returned {}: {}", status, body);
        }

        let payload = format!("firestream-e2e {}", std::process::id());
        let object = format!("{}/storage/v1/object/{}/probe.txt", gw, BUCKET);
        let resp = c
            .post(&object)
            .header("apikey", &keys.service)
            .header("Authorization", &bearer)
            .header("Content-Type", "text/plain")
            .header("x-upsert", "true")
            .body(payload.clone())
            .send()?;
        expect_status(resp, "upload", &[200])?;

        let resp = c
            .get(&object)
            .header("apikey", &keys.service)
            .header("Authorization", &bearer)
            .send()?;
        let got = expect_status(resp, "download", &[200])?;
        if got != payload {
            bail!("downloaded {:?}, uploaded {:?}", got, payload);
        }
        Ok(())
    }
}

/// Node script run in the `studio` target (the Studio image ships Node with a
/// global WebSocket). It joins a `postgres_changes` channel on the client key,
/// inserts a row through PostgREST once Realtime reports the subscription,
/// and exits 0 when the INSERT event arrives.
const REALTIME_SCRIPT: &str = r#"
const [gw, key, service, table] = process.argv.slice(1);
const note = `realtime-${Date.now()}`;
const ws = new WebSocket(`${gw.replace(/^http/, "ws")}/realtime/v1/websocket?apikey=${encodeURIComponent(key)}&vsn=1.0.0`);
const fail = (why) => { console.error(why); process.exit(1); };
setTimeout(() => fail("timed out waiting for the change event"), 30000);
ws.onerror = (e) => fail(`websocket error: ${e.message || e.type}`);
ws.onopen = () => ws.send(JSON.stringify({
  topic: "realtime:firestream-e2e", event: "phx_join", ref: "1",
  payload: { access_token: key, config: { postgres_changes: [{ event: "INSERT", schema: "public", table }] } },
}));
ws.onmessage = async (m) => {
  const msg = JSON.parse(m.data);
  if (msg.event === "phx_reply" && msg.payload.status !== "ok") fail(`join refused: ${m.data}`);
  if (msg.event === "system" && msg.payload.extension === "postgres_changes") {
    if (msg.payload.status !== "ok") fail(`subscription failed: ${m.data}`);
    const r = await fetch(`${gw}/rest/v1/${table}`, {
      method: "POST",
      headers: { apikey: service, Authorization: `Bearer ${service}`, "Content-Type": "application/json", Prefer: "return=minimal" },
      body: JSON.stringify({ note }),
    });
    if (!r.ok) fail(`insert returned ${r.status}: ${await r.text()}`);
  }
  if (msg.event === "postgres_changes" && msg.payload.data && msg.payload.data.record && msg.payload.data.record.note === note) {
    console.log("change event received");
    process.exit(0);
  }
};
"#;

/// Run [`REALTIME_SCRIPT`] with `client_key` as the websocket and join key.
fn realtime_change(s: &SupabaseStack, keys: &Keys, client_key: &str) -> Result<()> {
    s.ensure_table(keys)?;
    let out = s.exec.exec(
        &s.studio_target,
        &[
            "node",
            "-e",
            REALTIME_SCRIPT,
            &s.internal_gateway_url,
            client_key,
            &keys.service,
            TABLE,
        ],
    )?;
    if !out.status.success() {
        bail!(
            "realtime script exited {}: {}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Realtime delivers a `postgres_changes` INSERT over a websocket opened
/// with the legacy HS256 anon key.
pub struct RealtimeChange(pub Arc<SupabaseStack>);

impl RealtimeChange {
    pub const NAME: &'static str = "supabase_realtime_change";
}

impl Probe for RealtimeChange {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn run(&self) -> Result<()> {
        let keys = self.0.keys()?;
        realtime_change(&self.0, &keys, &keys.anon)
    }
}

/// Realtime delivers a `postgres_changes` INSERT over a websocket opened
/// with the `sb_publishable_*` key. The gateway swaps that key for the ES256
/// anon JWT, so this probe fails when Realtime's tenant has no JWKS.
pub struct RealtimePublishable(pub Arc<SupabaseStack>);

impl RealtimePublishable {
    pub const NAME: &'static str = "supabase_realtime_publishable";
}

impl Probe for RealtimePublishable {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn run(&self) -> Result<()> {
        let s = &self.0;
        let keys = s.keys()?;
        let publishable = s.printenv(&s.gateway_target, "SUPABASE_PUBLISHABLE_KEY")?;
        if !publishable.starts_with("sb_publishable_") {
            bail!("the gateway has no sb_publishable_* key (SUPABASE_PUBLISHABLE_KEY)");
        }
        realtime_change(s, &keys, &publishable)
    }
}

/// A query through Supavisor's transaction-mode port as `postgres.<tenant>`.
pub struct SupavisorQuery(pub Arc<SupabaseStack>);

impl SupavisorQuery {
    pub const NAME: &'static str = "supabase_supavisor_query";
}

impl Probe for SupavisorQuery {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn run(&self) -> Result<()> {
        let s = &self.0;
        let keys = s.keys()?;
        let got = s.psql(&keys, &s.supavisor_host, 6543, "postgres.firestream", "select 40 + 2")?;
        if got != "42" {
            bail!("query through supavisor returned {:?}", got);
        }
        Ok(())
    }
}

/// Studio answers behind the gateway's dashboard login, and refuses without it.
pub struct StudioDashboard(pub Arc<SupabaseStack>);

impl StudioDashboard {
    pub const NAME: &'static str = "supabase_studio";
}

impl Probe for StudioDashboard {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn run(&self) -> Result<()> {
        let keys = self.0.keys()?;
        let c = client()?;
        let url = format!("{}/api/platform/profile", self.0.gateway_url);
        let anonymous = c.get(&url).send()?.status().as_u16();
        if anonymous != 401 {
            bail!("studio without the dashboard login returned {}, not 401", anonymous);
        }
        let resp = c
            .get(&url)
            .basic_auth(&keys.dashboard_user, Some(&keys.dashboard_password))
            .send()?;
        expect_status(resp, "studio profile", &[200]).map(|_| ())
    }
}
