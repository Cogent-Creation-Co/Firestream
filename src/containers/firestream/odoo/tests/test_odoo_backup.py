"""End-to-end tests of the backup sets and of `firestream-odoo-call --dry-run`.

The check `firestream-odoo-backup` (nix/flake-modules/checks.nix) starts a
PostgreSQL server and fake-gcs-server, then runs this file. The database has
the few Odoo tables that the backup reads; Odoo itself is not needed.

Environment: ODOO_DATABASE_HOST/PORT/USER, TEST_WORK (a scratch directory),
TOOLS (the scripts directory), and STORAGE_EMULATOR_HOST for the GCS tests.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import unittest
from pathlib import Path

import psycopg2

TOOLS = Path(os.environ["TOOLS"])
WORK = Path(os.environ["TEST_WORK"])
sys.path.insert(0, str(TOOLS))

import odoo_tools  # noqa: E402

SOURCE_DB = "odoo_source"
# NOTE: the check starts PostgreSQL with a small lock table. These tables
# need more locks in one restore transaction than it holds, but fewer than
# pg_dump takes.
TABLES = 300
OBJECTS = WORK / "objects"
DATA_DIR = WORK / "data"
BACKUPS = WORK / "backups"


def sql(db: str, statement: str, params=None, fetch=False):
    conn = psycopg2.connect(
        host=os.environ["ODOO_DATABASE_HOST"], port=os.environ["ODOO_DATABASE_PORT_NUMBER"],
        user=os.environ["ODOO_DATABASE_USER"], dbname=db,
    )
    conn.autocommit = True
    try:
        with conn.cursor() as cr:
            cr.execute(statement, params)
            return cr.fetchall() if fetch else None
    finally:
        conn.close()


def tool(*args, env=None, ok=True) -> dict:
    """Run odoo_tools.py; return its JSON report line."""
    result = subprocess.run(
        [sys.executable, str(TOOLS / "odoo_tools.py"), *args],
        capture_output=True, text=True, env={**os.environ, **(env or {})},
    )
    lines = result.stdout.strip().splitlines()
    if len(lines) != 1:
        raise AssertionError(f"want one report line, got {result.stdout!r}\n{result.stderr}")
    report = json.loads(lines[0])
    if ok and (result.returncode != 0 or report["status"] != "ok"):
        raise AssertionError(f"{args} failed: {report}\n{result.stderr}")
    if not ok and (result.returncode == 0 or report["status"] != "error"):
        raise AssertionError(f"{args} should fail: {report}")
    return report


def make_source(storage_protocol: str = "file", storage_root: str | None = None, options: str = "{}"):
    sql("postgres", f"DROP DATABASE IF EXISTS {SOURCE_DB}")
    sql("postgres", f"CREATE DATABASE {SOURCE_DB}")
    root = storage_root or str(OBJECTS)
    sql(SOURCE_DB, """
        CREATE EXTENSION IF NOT EXISTS pg_trgm;
        CREATE EXTENSION IF NOT EXISTS unaccent;
        CREATE TABLE ir_module_module (name varchar, state varchar, latest_version varchar);
        INSERT INTO ir_module_module VALUES ('base', 'installed', '18.0.1.3'),
                                            ('fs_attachment', 'installed', '18.0.1.0.0'),
                                            ('sale', 'uninstalled', NULL);
        CREATE TABLE ir_config_parameter (key varchar, value text);
        INSERT INTO ir_config_parameter VALUES
            ('module_auto_update.installed_checksums', '{"base": "c0ffee"}');
        CREATE TABLE fs_storage (id serial, code varchar, protocol varchar,
                                 directory_path varchar, options text);
        CREATE TABLE ir_attachment (id serial PRIMARY KEY, name varchar,
                                    store_fname varchar, db_datas bytea);
        CREATE INDEX ir_attachment_name_trgm ON ir_attachment USING gin (name gin_trgm_ops);
    """)
    sql(SOURCE_DB, "INSERT INTO fs_storage (code, protocol, directory_path, options) VALUES (%s, %s, %s, %s)",
        ("platform_filestore", storage_protocol, root, options))
    sql(SOURCE_DB, """
        INSERT INTO ir_attachment (name, store_fname, db_datas) VALUES
            ('a', 'platform_filestore://aa/obj-a', NULL),
            ('b', 'platform_filestore://bb/obj-b', NULL),
            ('b copy', 'platform_filestore://bb/obj-b', NULL),
            ('c', 'platform_filestore://cc/obj-c', NULL),
            ('local', 'ab/abcdef', NULL),
            ('in db', NULL, 'x'::bytea);
    """)
    local = DATA_DIR / "filestore" / SOURCE_DB / "ab"
    local.mkdir(parents=True, exist_ok=True)
    (local / "abcdef").write_text("local file")


def put_objects():
    for path, content in (("aa/obj-a", "A"), ("bb/obj-b", "B"), ("cc/obj-c", "C")):
        target = OBJECTS / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)


def location(prefix: str) -> list[str]:
    return ["--target", "file", "--bucket", str(BACKUPS), "--prefix", prefix, "--env", "test"]


def database_names() -> set[str]:
    return {row[0] for row in sql("postgres", "SELECT datname FROM pg_database", fetch=True)}


def storage_row(db: str):
    return sql(db, "SELECT protocol, directory_path FROM fs_storage WHERE code = 'platform_filestore'", fetch=True)


RESTORED = WORK / "restored"
ROOT = ["--storage-root", f"platform_filestore=file://{RESTORED}"]
COPY = [*ROOT, "--filestore-bucket", f"file://{RESTORED}"]


class FileTargetTest(unittest.TestCase):
    """The set format, verification and restore, on the local file protocol."""

    def setUp(self):
        for path in (OBJECTS, DATA_DIR, BACKUPS, RESTORED):
            shutil.rmtree(path, ignore_errors=True)
        for name in database_names() - {"postgres", "template0", "template1"}:
            sql("postgres", f'DROP DATABASE "{name}"')
        put_objects()
        make_source()
        os.environ.update({
            "ODOO_DATABASE_NAME": SOURCE_DB, "ODOO_DATA_DIR": str(DATA_DIR),
            "ODOO_VERSION": "18.0", "FIRESTREAM_ODOO_CODE_ID": "code-123",
            "FIRESTREAM_ODOO_IMAGE_DIGEST": "sha256:abc",
        })

    def backup(self, *extra, ok=True, prefix="sets"):
        return tool("backup", *location(prefix), "--filestore-mode", "objectManifest", *extra, ok=ok)

    def set_dir(self, report) -> Path:
        return Path(report["set"].removeprefix("file://"))

    def test_object_manifest_set_and_restore(self):
        report = self.backup("--tag", "pre-release")
        set_dir = self.set_dir(report)
        self.assertEqual(set_dir.parent, BACKUPS / "sets" / "test")
        self.assertEqual(
            sorted(p.name for p in set_dir.iterdir()),
            ["db.dump", "filestore.json", "filestore.tar.gz", "manifest.json"],
        )
        manifest = json.loads((set_dir / "manifest.json").read_text())
        self.assertEqual(manifest["tag"], "pre-release")
        self.assertEqual(manifest["database"], SOURCE_DB)
        self.assertEqual(manifest["code_id"], "code-123")
        self.assertEqual(manifest["image_digest"], "sha256:abc")
        self.assertEqual(manifest["odoo_version"], "18.0")
        self.assertEqual(manifest["module_checksums"], {"base": "c0ffee"})
        self.assertEqual(manifest["modules"], {"base": "18.0.1.3", "fs_attachment": "18.0.1.0.0"})
        self.assertEqual(manifest["object_count"], 3)
        self.assertEqual(manifest["attachments"],
                         {"total": 6, "in_storage": 4, "local": 1, "in_database": 1, "objects": 3})
        self.assertGreater(manifest["database_size_bytes"], 0)
        dump_sha = hashlib.sha256((set_dir / "db.dump").read_bytes()).hexdigest()
        self.assertEqual(manifest["files"]["db.dump"]["sha256"], dump_sha)
        self.assertEqual(manifest["files"]["db.dump"]["verified_by"], "read-back-sha256")
        self.assertEqual(report["db_dump_sha256"], dump_sha)
        self.assertEqual((set_dir / "db.dump").read_bytes()[:5], b"PGDMP")
        listing = json.loads((set_dir / "filestore.json").read_text())
        self.assertEqual(listing["count"], 3)
        self.assertEqual(sorted(entry["path"] for entry in listing["objects"]),
                         ["aa/obj-a", "bb/obj-b", "cc/obj-c"])

        restored = tool("restore", "--set", report["set"], "--database", "restored_a", *COPY)
        self.assertEqual(restored["attachments"], 6)
        self.assertEqual(restored["objects"]["copied"], 3)
        self.assertFalse(restored["replaced"])
        self.assertEqual((RESTORED / "bb" / "obj-b").read_text(), "B")
        self.assertEqual((DATA_DIR / "filestore" / "restored_a" / "ab" / "abcdef").read_text(), "local file")
        self.assertEqual(sql("restored_a", "SELECT count(*) FROM ir_attachment", fetch=True), [(6,)])
        self.assertEqual(storage_row("restored_a"), [("file", str(RESTORED))])
        self.assertEqual(storage_row(SOURCE_DB), [("file", str(OBJECTS))])
        self.assertEqual(
            sql("restored_a", "SELECT count(*) FROM pg_indexes WHERE indexname = 'ir_attachment_name_trgm'",
                fetch=True), [(1,)])
        self.assertEqual(sql("restored_a", "SELECT extname FROM pg_extension WHERE extname = 'unaccent'",
                             fetch=True), [("unaccent",)])
        self.assertEqual({name for name in database_names() if "_restore_" in name}, set())

        (RESTORED / "cc" / "obj-c").write_text("newer")
        again = tool("restore", "--set", report["set"], "--database", "restored_b", *COPY)
        self.assertEqual(again["objects"]["copied"], 0)
        self.assertEqual(again["objects"]["already_present"], 3)
        self.assertEqual((RESTORED / "cc" / "obj-c").read_text(), "newer")

    def test_storage_roots_must_be_explicit(self):
        report = self.backup()
        refused = tool("restore", "--set", report["set"], "--database", "restored_a", ok=False)
        self.assertIn("--storage-root CODE=URI", refused["error"])
        refused = tool("restore", "--set", report["set"], "--database", "restored_a",
                       "--filestore-bucket", f"file://{RESTORED}", ok=False)
        self.assertIn("needs --storage-root platform_filestore=", refused["error"])
        refused = tool("restore", "--set", report["set"], "--database", "restored_a",
                       "--storage-root", "other=file:///x", ok=False)
        self.assertIn("no fs.storage with the code other", refused["error"])
        self.assertEqual({name for name in database_names() if name.startswith("restored_a")}, set())

        kept = tool("restore", "--set", report["set"], "--database", "restored_a", "--keep-storage-roots")
        self.assertEqual(kept["storage_roots"], {})
        self.assertEqual(storage_row("restored_a"), [("file", str(OBJECTS))])

    def test_replace_swaps_and_keeps_the_old_database(self):
        report = self.backup()
        sql(SOURCE_DB, "INSERT INTO ir_attachment (name) VALUES ('after the backup')")
        refused = tool("restore", "--set", report["set"], "--database", SOURCE_DB, *ROOT, ok=False)
        self.assertIn("exists", refused["error"])
        self.assertEqual(sql(SOURCE_DB, "SELECT count(*) FROM ir_attachment", fetch=True), [(7,)])

        client = psycopg2.connect(host=os.environ["ODOO_DATABASE_HOST"],
                                  port=os.environ["ODOO_DATABASE_PORT_NUMBER"],
                                  user=os.environ["ODOO_DATABASE_USER"], dbname=SOURCE_DB)
        try:
            replaced = tool("restore", "--set", report["set"], "--database", SOURCE_DB, "--replace", *ROOT)
            with self.assertRaises(psycopg2.Error):
                client.cursor().execute("SELECT 1")
        finally:
            client.close()
        self.assertTrue(replaced["replaced"])
        before = replaced["previous_database"]
        self.assertTrue(before.startswith(f"{SOURCE_DB}_before_"))
        self.assertEqual(sql(SOURCE_DB, "SELECT count(*) FROM ir_attachment", fetch=True), [(6,)])
        self.assertEqual(storage_row(SOURCE_DB), [("file", str(RESTORED))])
        self.assertEqual(sql("postgres", "SELECT datallowconn FROM pg_database WHERE datname = %s",
                             (before,), fetch=True), [(False,)])
        sql("postgres", f'ALTER DATABASE "{before}" ALLOW_CONNECTIONS true')
        self.assertEqual(sql(before, "SELECT count(*) FROM ir_attachment", fetch=True), [(7,)])
        self.assertEqual((DATA_DIR / "filestore" / SOURCE_DB / "ab" / "abcdef").read_text(), "local file")
        self.assertTrue((DATA_DIR / "filestore" / before / "ab" / "abcdef").exists())

    def test_neutralize_runs_on_the_prepared_copy_before_it_gets_its_name(self):
        import argparse
        import odoo_backup

        report = self.backup()
        seen = {}

        def neutralize(db):
            seen["db"] = db
            seen["storage"] = storage_row(db)
            seen["target_exists"] = "restored_a" in database_names()

        args = argparse.Namespace(
            set=report["set"], archive=None, database="restored_a", storage_root=ROOT[1:],
            keep_storage_roots=False, filestore_bucket=None, neutralize=True, replace=False, force=False,
            jobs=2, maintenance_db="postgres", target="file", bucket=str(BACKUPS), prefix="sets",
            environment="test",
        )
        result = odoo_backup.restore(args, neutralize=neutralize)
        self.assertTrue(seen["db"].startswith("restored_a_restore_"))
        self.assertEqual(seen["storage"], [("file", str(RESTORED))])
        self.assertFalse(seen["target_exists"])
        self.assertTrue(result["neutralized"])

    def test_latest_resolves_the_newest_complete_set(self):
        first = self.backup()
        newer = self.set_dir(first).parent / "29990101T000000Z"
        shutil.copytree(self.set_dir(first), newer)
        (newer / "manifest.json").unlink()
        latest = tool("backups", "list", "--latest", *location("sets"))
        self.assertEqual(latest["latest"]["set"], first["set"])
        restored = tool("restore", "--set", "latest", *location("sets"), "--database", "restored_a", *ROOT)
        self.assertEqual(restored["source"], first["set"])
        refused = tool("backups", "list", "--latest", *location("none"), ok=False)
        self.assertIn("no complete backup set", refused["error"])

    def test_restore_refuses_a_set_without_manifest(self):
        report = self.backup()
        incomplete = self.set_dir(report).parent / "20000101T000000Z"
        shutil.copytree(self.set_dir(report), incomplete)
        (incomplete / "manifest.json").unlink()
        refused = tool("restore", "--set", f"file://{incomplete}", "--database", "restored_a", *ROOT, ok=False)
        self.assertIn("no manifest.json", refused["error"])
        self.assertNotIn("restored_a", database_names())

        listed = tool("backups", "list", *location("sets"))
        self.assertEqual(listed["count"], 2)
        self.assertEqual([entry["complete"] for entry in listed["sets"]], [False, True])
        self.assertEqual(listed["sets"][1]["db_dump_sha256"], report["db_dump_sha256"])

    def test_restore_refuses_a_changed_dump_and_another_major_version(self):
        report = self.backup()
        refused = tool("restore", "--set", report["set"], "--database", "restored_a", *ROOT,
                       env={"ODOO_VERSION": "17.0"}, ok=False)
        self.assertIn("Odoo 18.0", refused["error"])

        no_version = self.set_dir(report).parent / "20000101T000000Z"
        shutil.copytree(self.set_dir(report), no_version)
        manifest = json.loads((no_version / "manifest.json").read_text())
        manifest["odoo_version"] = None
        (no_version / "manifest.json").write_text(json.dumps(manifest))
        refused = tool("restore", "--set", f"file://{no_version}", "--database", "restored_a", *ROOT,
                       env={"ODOO_VERSION": "17.0"}, ok=False)
        self.assertIn("Odoo 18.0.1.3", refused["error"])

        dump = self.set_dir(report) / "db.dump"
        dump.write_bytes(dump.read_bytes() + b"x")
        refused = tool("restore", "--set", report["set"], "--database", "restored_a", *ROOT, ok=False)
        self.assertIn("sha256", refused["error"])

    def test_a_failed_count_check_drops_the_copy(self):
        report = self.backup()
        wrong = self.set_dir(report).parent / "20000101T000000Z"
        shutil.copytree(self.set_dir(report), wrong)
        manifest = json.loads((wrong / "manifest.json").read_text())
        manifest["attachments"]["total"] = 99
        (wrong / "manifest.json").write_text(json.dumps(manifest))
        refused = tool("restore", "--set", f"file://{wrong}", "--database", "restored_a", *ROOT, ok=False)
        self.assertIn("the manifest says 99", refused["error"])
        self.assertEqual({name for name in database_names() if name.startswith("restored_a")}, set())
        self.assertEqual(list((DATA_DIR / "filestore").glob("restored_a*")), [])

    def test_a_missing_generation_stops_the_restore_before_the_database(self):
        report = self.backup()
        (OBJECTS / "bb" / "obj-b").unlink()
        refused = tool("restore", "--set", report["set"], "--database", "restored_a", *COPY, ok=False)
        self.assertIn("1 listed object generations are not in the source storage", refused["error"])
        self.assertEqual({name for name in database_names() if name.startswith("restored_a")}, set())
        self.assertFalse(RESTORED.exists())

    def test_a_set_never_replaces_an_object(self):
        import odoo_backup

        report = self.backup()
        store = odoo_backup.open_store(report["set"])
        with self.assertRaises(odoo_backup.BackupError):
            store.put_bytes("db.dump", b"probe")
        self.assertEqual(hashlib.sha256((self.set_dir(report) / "db.dump").read_bytes()).hexdigest(),
                         report["db_dump_sha256"])

        def broken(stream):
            stream.write(b"half")
            raise RuntimeError("the producer failed")

        with self.assertRaises(RuntimeError):
            store.put("partial.bin", broken)
        self.assertFalse((self.set_dir(report) / "partial.bin").exists())

    def test_missing_files_fail_the_set(self):
        (OBJECTS / "cc" / "obj-c").unlink()
        failed = self.backup(ok=False)
        self.assertIn("1 referenced objects are not in their storage", failed["error"])
        self.assertEqual(list(BACKUPS.rglob("manifest.json")), [])
        allowed = self.backup("--allow-missing-objects", prefix="allowed")
        self.assertEqual(allowed["object_count"], 2)

        put_objects()
        (DATA_DIR / "filestore" / SOURCE_DB / "ab" / "abcdef").unlink()
        failed = self.backup(ok=False, prefix="local")
        self.assertIn("ab/abcdef", failed["error"])

    def test_a_large_schema_restores(self):
        conn = psycopg2.connect(host=os.environ["ODOO_DATABASE_HOST"], port=os.environ["ODOO_DATABASE_PORT_NUMBER"],
                                user=os.environ["ODOO_DATABASE_USER"], dbname=SOURCE_DB)
        conn.autocommit = True
        try:
            with conn.cursor() as cr:
                for index in range(TABLES):
                    cr.execute(f"CREATE TABLE wide_{index} (id serial PRIMARY KEY, name varchar UNIQUE)")
        finally:
            conn.close()
        report = self.backup()
        dump = WORK / "large.dump"
        shutil.copy(self.set_dir(report) / "db.dump", dump)
        sql("postgres", "CREATE DATABASE single_transaction")
        single = subprocess.run(
            ["pg_restore", "--no-owner", "--single-transaction", "--exit-on-error", "-d", "single_transaction",
             "-h", os.environ["ODOO_DATABASE_HOST"], "-p", os.environ["ODOO_DATABASE_PORT_NUMBER"],
             "-U", os.environ["ODOO_DATABASE_USER"], str(dump)],
            capture_output=True, text=True,
        )
        self.assertNotEqual(single.returncode, 0)
        self.assertIn("max_locks_per_transaction", single.stderr)
        restored = tool("restore", "--set", report["set"], "--database", "restored_a", *ROOT)
        self.assertEqual(restored["attachments"], 6)
        self.assertEqual(sql("restored_a", "SELECT count(*) FROM pg_tables WHERE tablename LIKE 'wide_%%'",
                             fetch=True), [(TABLES,)])

    def test_archive_mode_and_odoo_dump_archive(self):
        report = tool("backup", *location("archives"), "--filestore-mode", "archive", "--tag", "nightly")
        set_dir = self.set_dir(report)
        self.assertEqual(sorted(p.name for p in set_dir.iterdir()),
                         ["db.dump", "filestore.tar.gz", "manifest.json"])
        tool("restore", "--set", report["set"], "--database", "restored_a", "--keep-storage-roots")
        self.assertEqual((DATA_DIR / "filestore" / "restored_a" / "ab" / "abcdef").read_text(), "local file")

        archive = WORK / "odoo-dump.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(set_dir / "db.dump", arcname="db.dump")
            (WORK / "legacy.json").write_text(json.dumps({"odoo_dump": "2", "version": "18.0"}))
            tar.add(WORK / "legacy.json", arcname="manifest.json")
            tar.add(DATA_DIR / "filestore" / SOURCE_DB, arcname="filestore")
        restored = tool("restore", "--archive", str(archive), "--database", "restored_c", "--keep-storage-roots")
        self.assertEqual(restored["attachments"], 6)
        self.assertEqual(restored["filestore_files"], 1)

    def test_bad_tag_is_refused(self):
        failed = self.backup("--tag", "weekly", ok=False)
        self.assertIn("--tag must be one of", failed["error"])


class LiveVersionTest(unittest.TestCase):
    """The generation of an object at the snapshot time."""

    def test_the_generation_live_at_the_snapshot_wins(self):
        import datetime as dt
        import odoo_backup

        versions = [
            {"generation": "1", "timeCreated": "2026-10-14T00:00:00Z", "timeDeleted": "2026-10-14T01:00:00Z"},
            {"generation": "2", "timeCreated": "2026-10-14T01:00:00Z", "timeDeleted": "2026-10-14T03:00:00Z"},
            {"generation": "3", "timeCreated": "2026-10-14T03:00:00Z"},
        ]
        at = lambda text: dt.datetime.fromisoformat(text)  # noqa: E731
        live = odoo_backup.live_version
        self.assertEqual(live(versions, at("2026-10-14T00:30:00+00:00"))["generation"], "1")
        self.assertEqual(live(versions, at("2026-10-14T02:00:00+00:00"))["generation"], "2")
        self.assertEqual(live(versions, at("2026-10-14T05:00:00+00:00"))["generation"], "3")
        self.assertEqual(live(versions, at("2026-10-14T02:59:00+00:00"))["generation"], "2")
        self.assertEqual(live(versions[2:], at("2026-10-14T02:59:00+00:00"))["generation"], "3")
        self.assertIsNone(live(versions[2:], at("2026-10-14T02:00:00+00:00")))


@unittest.skipUnless(os.environ.get("STORAGE_EMULATOR_HOST"), "no fake-gcs-server")
class GcsTargetTest(unittest.TestCase):
    """The gcs target and generation restore, against fake-gcs-server."""

    def test_gcs_set_restores_the_recorded_generation(self):
        from google.cloud import storage

        client = storage.Client()
        files = client.bucket("files")
        files.versioning_enabled = True
        client.create_bucket(files)
        client.create_bucket("backups")
        client.create_bucket("staging-files")
        generations = {}
        for path, content in (("dir/aa/obj-a", b"A"), ("dir/bb/obj-b", b"B"), ("dir/cc/obj-c", b"C")):
            blob = files.blob(path)
            blob.upload_from_string(content)
            generations[path.removeprefix("dir/")] = str(blob.generation)
        shutil.rmtree(DATA_DIR, ignore_errors=True)
        make_source("gcs", "files/dir", '{"token": "anon"}')
        sql(SOURCE_DB, "DELETE FROM ir_attachment WHERE store_fname NOT LIKE '%%://%%' OR store_fname IS NULL")
        os.environ.update({"ODOO_DATABASE_NAME": SOURCE_DB, "ODOO_DATA_DIR": str(DATA_DIR),
                           "ODOO_VERSION": "18.0"})
        for name in ("restored_gcs", "restored_gone"):
            sql("postgres", f"DROP DATABASE IF EXISTS {name}")

        report = tool("backup", "--target", "gcs", "--bucket", "backups", "--prefix", "odoo-backups",
                      "--env", "production", "--filestore-mode", "objectManifest", "--tag", "nightly")
        self.assertTrue(report["set"].startswith("gs://backups/odoo-backups/production/"))
        prefix = report["set"].removeprefix("gs://backups/")
        manifest = json.loads(client.bucket("backups").blob(f"{prefix}/manifest.json").download_as_bytes())
        self.assertEqual(manifest["files"]["db.dump"]["verified_by"], "gcs-md5")
        self.assertNotIn("filestore.tar.gz", manifest["files"])
        listing = json.loads(client.bucket("backups").blob(f"{prefix}/filestore.json").download_as_bytes())
        self.assertEqual({entry["path"]: entry["generation"] for entry in listing["objects"]}, generations)

        files.blob("dir/bb/obj-b").upload_from_string(b"B, edited after the backup")
        files.blob("dir/cc/obj-c").delete()

        latest = tool("backups", "list", "--latest", "--target", "gcs", "--bucket", "backups",
                      "--prefix", "odoo-backups", "--env", "production")
        self.assertEqual(latest["latest"]["set"], report["set"])

        target = "gs://staging-files/dir"
        restored = tool("restore", "--set", "latest", "--target", "gcs", "--bucket", "backups",
                        "--prefix", "odoo-backups", "--env", "production", "--database", "restored_gcs",
                        "--storage-root", f"platform_filestore={target}", "--filestore-bucket", target)
        self.assertEqual(restored["objects"]["copied"], 3)
        staging = client.bucket("staging-files")
        self.assertEqual(staging.blob("dir/bb/obj-b").download_as_bytes(), b"B")
        self.assertEqual(staging.blob("dir/cc/obj-c").download_as_bytes(), b"C")
        self.assertEqual(storage_row("restored_gcs"), [("gcs", "staging-files/dir")])

        # NOTE: fake-gcs-server keeps serving a generation that a delete
        # names, so this set lists a generation that never existed.
        backups = client.bucket("backups")
        listing["objects"][0]["generation"] = "1"
        changed = json.dumps(listing).encode()
        manifest["files"]["filestore.json"]["sha256"] = hashlib.sha256(changed).hexdigest()
        gone = "odoo-backups/production/20000101T000000Z"
        backups.blob(f"{gone}/filestore.json").upload_from_string(changed)
        backups.blob(f"{gone}/manifest.json").upload_from_string(json.dumps(manifest))
        backups.copy_blob(backups.blob(f"{prefix}/db.dump"), backups, f"{gone}/db.dump")
        refused = tool("restore", "--set", f"gs://backups/{gone}", "--database", "restored_gone",
                       "--storage-root", "platform_filestore=gs://staging-files/other",
                       "--filestore-bucket", "gs://staging-files/other", ok=False)
        self.assertIn("1 listed object generations are not in the source storage", refused["error"])
        self.assertNotIn("restored_gone", database_names())


class FakeCursor:
    commits = 0

    def commit(self):
        type(self).commits += 1


class FakeCr:
    def __init__(self):
        self.cursor = FakeCursor()
        self.rolled_back = False
        self.statements = []

    def execute(self, statement):
        self.statements.append(statement)

    def commit(self):
        self.cursor.commit()

    def rollback(self):
        self.rolled_back = True


class FakeModel:
    def __init__(self, cr):
        self.cr = cr
        self.calls = []

    def _platform_storage_migrate(self, dry_run=False, batch_size=100):
        self.calls.append(dry_run)
        if dry_run:
            return {"dry_run": True, "to_move": 522}
        self.cr.commit()
        return {"dry_run": False, "moved": 522}

    def no_flag(self):
        self.calls.append("no_flag")
        return True

    def ignores_flag(self, dry_run=False):
        self.cr.commit()
        return "committed"


class FakeEnv(dict):
    def __init__(self):
        self.cr = FakeCr()
        super().__init__({"ir.attachment": FakeModel(self.cr)})


class CallDryRunTest(unittest.TestCase):
    """`firestream-odoo-call --dry-run` must write nothing."""

    def setUp(self):
        FakeCursor.commits = 0
        self.env = FakeEnv()
        self.model = self.env["ir.attachment"]

    def invoke(self, method, kwargs, dry_run):
        return odoo_tools.invoke(self.env, "ir.attachment", method, kwargs, dry_run, FakeCursor)

    def test_dry_run_reaches_a_method_that_commits(self):
        result, dry_run = self.invoke("_platform_storage_migrate", {}, True)
        self.assertTrue(dry_run)
        self.assertEqual(result, {"dry_run": True, "to_move": 522})
        self.assertEqual(self.model.calls, [True])
        self.assertEqual(FakeCursor.commits, 0)
        self.assertTrue(self.env.cr.rolled_back)
        self.assertEqual(self.env.cr.statements, ["SET TRANSACTION READ ONLY"])

    def test_method_without_dry_run_is_refused(self):
        with self.assertRaises(odoo_tools.SyncError):
            self.invoke("no_flag", {}, True)
        self.assertEqual(self.model.calls, [])

    def test_conflicting_kwargs_are_refused(self):
        with self.assertRaises(odoo_tools.SyncError):
            self.invoke("_platform_storage_migrate", {"dry_run": False}, True)
        self.assertEqual(self.model.calls, [])

    def test_a_commit_during_a_dry_run_fails_and_rolls_back(self):
        with self.assertRaises(odoo_tools.CommitRefused):
            self.invoke("ignores_flag", {}, True)
        self.assertEqual(FakeCursor.commits, 0)
        self.assertTrue(self.env.cr.rolled_back)
        self.env.cr.commit()
        self.assertEqual(FakeCursor.commits, 1)

    def test_kwargs_dry_run_alone_is_a_dry_run(self):
        _, dry_run = self.invoke("_platform_storage_migrate", {"dry_run": True}, False)
        self.assertTrue(dry_run)
        self.assertEqual(FakeCursor.commits, 0)

    def test_a_real_call_commits(self):
        result, dry_run = self.invoke("_platform_storage_migrate", {"batch_size": 5}, False)
        self.assertFalse(dry_run)
        self.assertEqual(result["moved"], 522)
        self.assertEqual(FakeCursor.commits, 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
