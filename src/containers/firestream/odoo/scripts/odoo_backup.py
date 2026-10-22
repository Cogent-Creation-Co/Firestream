"""Backup sets of the Firestream Odoo image.

A backup set is one directory, `<prefix>/<environment>/<UTC timestamp>/`, in
an object store:

- `db.dump`: `pg_dump --format=custom` of the database (compressed).
- `filestore.tar.gz`: the local filestore (`archive` mode), or the local
  attachment files that `objectManifest` mode finds.
- `filestore.json`: each attachment object in an `fs.storage`, with the GCS
  generation that was live at the dump snapshot (`objectManifest` mode).
- `manifest.json`: written last. A set without it is incomplete, and a
  restore refuses it.

This module does not import Odoo. `odoo_tools.py` gives it the commands and
the Odoo calls (neutralize) that need Odoo.
"""

from __future__ import annotations

import base64
import datetime as dt
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path, PurePosixPath
from urllib.parse import urlsplit

SET_FORMAT = "firestream-odoo-backup/1"
TAGS = ("nightly", "pre-release", "pre-restore", "manual")
FILESTORE_MODES = ("archive", "objectManifest")
TARGETS = ("gcs", "file")
TIMESTAMP_FORMAT = "%Y%m%dT%H%M%SZ"
CHECKSUMS_PARAM = "module_auto_update.installed_checksums"
SCHEMES = {"gs": "gcs", "gcs": "gcs", "file": "file", "": "file"}
# NOTE: the database clock and the GCS clock differ a little. An object that
# GCS dates up to this long after the snapshot still counts as live at it.
CLOCK_SKEW = dt.timedelta(minutes=2)
COPY_WORKERS = 16

DB_DUMP = "db.dump"
FILESTORE_ARCHIVE = "filestore.tar.gz"
FILESTORE_OBJECTS = "filestore.json"
MANIFEST = "manifest.json"


class BackupError(RuntimeError):
    """The backup or the restore cannot go on; the message tells why."""


def env_value(name: str, default: str = "") -> str:
    value = os.environ.get(name, "")
    if not value and os.environ.get(f"{name}_FILE"):
        value = Path(os.environ[f"{name}_FILE"]).read_text().strip()
    return value or default


def log(message: str) -> None:
    sys.stderr.write(f"[firestream] {message}\n")
    sys.stderr.flush()


def utc_now() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0)


def iso(moment: dt.datetime) -> str:
    return moment.strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_time(value: str | None) -> dt.datetime | None:
    if not value:
        return None
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def file_digests(path: Path) -> dict:
    writer = HashingWriter(None)
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            writer.write(block)
    return writer.digests()


def major(version: str | None) -> str:
    return (version or "").split(".", 1)[0].split("-", 1)[0]


def split_uri(uri: str) -> tuple[str, str]:
    """Return the fsspec protocol and the path of a URI (`gs://b/d` is `gcs`, `b/d`)."""
    parts = urlsplit(uri)
    protocol = SCHEMES.get(parts.scheme, parts.scheme)
    if protocol == "file":
        return protocol, parts.path.rstrip("/") or "/"
    return protocol, f"{parts.netloc}{parts.path}".strip("/")


class HashingWriter:
    """A write-only stream that hashes what it passes to `target`."""

    def __init__(self, target):
        self.target = target
        self.sha256 = hashlib.sha256()
        self.md5 = hashlib.md5()
        self.size = 0

    def write(self, data) -> int:
        self.sha256.update(data)
        self.md5.update(data)
        self.size += len(data)
        if self.target is not None:
            self.target.write(data)
        return len(data)

    def tell(self) -> int:
        return self.size

    def flush(self) -> None:
        pass

    def digests(self) -> dict:
        return {
            "sha256": self.sha256.hexdigest(),
            "md5": base64.b64encode(self.md5.digest()).decode(),
            "size": self.size,
        }


# ---------------------------------------------------------------- stores


class Store:
    """A directory in an object store. Every name is relative to `uri`."""

    uri: str

    def put(self, name: str, produce) -> dict:
        """Create `name` from what `produce(stream)` writes. Return the digests."""
        raise NotImplementedError

    def put_bytes(self, name: str, data: bytes) -> dict:
        return self.put(name, lambda stream: stream.write(data))

    def get_file(self, name: str, local: Path) -> None:
        raise NotImplementedError

    def read_bytes(self, name: str) -> bytes | None:
        raise NotImplementedError

    def list_dirs(self, name: str = "") -> list[str]:
        raise NotImplementedError

    def child(self, name: str) -> "Store":
        raise NotImplementedError

    def read_json(self, name: str) -> dict | None:
        data = self.read_bytes(name)
        return None if data is None else json.loads(data)


class GcsStore(Store):
    """A GCS prefix, through google-cloud-storage and Workload Identity."""

    def __init__(self, bucket: str, root: str, client=None):
        from google.cloud import storage

        self.client = client or storage.Client()
        self.bucket_name = bucket
        self.bucket = self.client.bucket(bucket)
        self.root = root.strip("/")
        self.uri = f"gs://{bucket}/{self.root}".rstrip("/")

    def key(self, name: str) -> str:
        return "/".join(part for part in (self.root, name.strip("/")) if part)

    def child(self, name: str) -> "GcsStore":
        return GcsStore(self.bucket_name, self.key(name), self.client)

    def put(self, name: str, produce) -> dict:
        from google.api_core.exceptions import GoogleAPICallError
        from google.cloud.storage.exceptions import DataCorruption

        blob = self.bucket.blob(self.key(name))
        # NOTE: the backup account holds roles/storage.objectCreator only. It
        # can create an object but not read it back. So the upload computes
        # the md5 of the bytes it sends, and the library compares it with the
        # md5 that GCS stored; generation 0 makes the upload fail rather than
        # replace an object of an older set. A failure inside `produce`
        # cancels the resumable upload, and no object appears.
        try:
            with blob.open("wb", if_generation_match=0, checksum="md5", timeout=600,
                           ignore_flush=True) as raw:
                stream = HashingWriter(raw)
                produce(stream)
        except DataCorruption as error:
            raise BackupError(f"GCS stored {self.key(name)} with another md5: {error}") from None
        except GoogleAPICallError as error:
            raise BackupError(f"upload of {self.key(name)} failed: {error}") from None
        return {**stream.digests(), "verified_by": "gcs-md5"}

    def get_file(self, name: str, local: Path) -> None:
        self.bucket.blob(self.key(name)).download_to_filename(str(local), timeout=600)

    def read_bytes(self, name: str) -> bytes | None:
        from google.api_core.exceptions import NotFound

        try:
            return self.bucket.blob(self.key(name)).download_as_bytes()
        except NotFound:
            return None

    def list_dirs(self, name: str = "") -> list[str]:
        prefix = self.key(name)
        prefix = f"{prefix}/" if prefix else ""
        pages = self.client.list_blobs(self.bucket_name, prefix=prefix, delimiter="/")
        for _page in pages.pages:
            pass
        return sorted(p[len(prefix):].rstrip("/") for p in pages.prefixes)


class FsspecStore(Store):
    """Any fsspec protocol. `file` serves the tests and a local disk."""

    def __init__(self, protocol: str, root: str):
        import fsspec

        self.protocol = protocol
        self.fs = fsspec.filesystem(protocol)
        self.root = root.rstrip("/") or "/"
        self.uri = f"{protocol}://{self.root}"

    def path(self, name: str) -> str:
        return f"{self.root}/{name.strip('/')}" if name else self.root

    def child(self, name: str) -> "FsspecStore":
        return FsspecStore(self.protocol, self.path(name))

    def put(self, name: str, produce) -> dict:
        path = self.path(name)
        if self.fs.exists(path):
            raise BackupError(f"{self.protocol}://{path} exists; a backup set never replaces an object")
        self.fs.makedirs(self.root, exist_ok=True)
        try:
            with self.fs.open(path, "wb") as raw:
                stream = HashingWriter(raw)
                produce(stream)
        except BaseException:
            self.fs.rm(path)
            raise
        digests = stream.digests()
        reread = HashingWriter(None)
        with self.fs.open(path, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                reread.write(block)
        if reread.digests()["sha256"] != digests["sha256"]:
            raise BackupError(f"{self.protocol}://{path} reads back with another sha256")
        return {**digests, "verified_by": "read-back-sha256"}

    def get_file(self, name: str, local: Path) -> None:
        self.fs.get_file(self.path(name), str(local))

    def read_bytes(self, name: str) -> bytes | None:
        try:
            return self.fs.cat_file(self.path(name))
        except FileNotFoundError:
            return None

    def list_dirs(self, name: str = "") -> list[str]:
        try:
            entries = self.fs.ls(self.path(name), detail=True)
        except FileNotFoundError:
            return []
        return sorted(
            PurePosixPath(entry["name"]).name for entry in entries if entry["type"] == "directory"
        )


def open_store(uri: str) -> Store:
    if "://" not in uri:
        raise BackupError(f"{uri!r} is not a URI; use gs://bucket/path or file:///path")
    protocol, path = split_uri(uri)
    if protocol == "gcs":
        bucket, _, root = path.partition("/")
        return GcsStore(bucket, root)
    return FsspecStore(protocol, path)


def base_uri(target: str, bucket: str, prefix: str) -> str:
    if not bucket:
        raise BackupError("no backup bucket; set FIRESTREAM_BACKUP_BUCKET or --bucket")
    if target not in TARGETS:
        raise BackupError(f"the backup target must be one of {', '.join(TARGETS)}, not {target!r}")
    scheme = {"gcs": "gs://", "file": "file://"}[target]
    if target == "file" and not bucket.startswith("/"):
        raise BackupError("the file target needs an absolute directory as the bucket")
    return f"{scheme}{bucket.rstrip('/')}/{prefix.strip('/')}".rstrip("/")


# ---------------------------------------------------------------- database


def pg_params(dbname: str) -> dict:
    return {
        "host": env_value("ODOO_DATABASE_HOST", "postgresql"),
        "port": env_value("ODOO_DATABASE_PORT_NUMBER", "5432"),
        "user": env_value("ODOO_DATABASE_USER", "firestream"),
        "password": env_value("ODOO_DATABASE_PASSWORD"),
        "dbname": dbname,
    }


def pg_env(dbname: str) -> dict:
    params = pg_params(dbname)
    return {
        **os.environ,
        "PGHOST": params["host"],
        "PGPORT": params["port"],
        "PGUSER": params["user"],
        "PGPASSWORD": params["password"],
        "PGDATABASE": dbname,
    }


def connect(dbname: str):
    import psycopg2

    return psycopg2.connect(**pg_params(dbname))


def run(command: list[str], dbname: str) -> None:
    log(" ".join(command))
    result = subprocess.run(command, env=pg_env(dbname), stdout=sys.stderr, stderr=sys.stderr)
    if result.returncode != 0:
        raise BackupError(f"{command[0]} exited with status {result.returncode}")


def stream_command(command: list[str], dbname: str):
    """Return a `produce` function that streams the stdout of `command`."""

    def produce(stream) -> None:
        log(" ".join(command))
        process = subprocess.Popen(command, env=pg_env(dbname), stdout=subprocess.PIPE, stderr=sys.stderr)
        try:
            for block in iter(lambda: process.stdout.read(1 << 20), b""):
                stream.write(block)
        finally:
            process.stdout.close()
            code = process.wait()
        if code != 0:
            raise BackupError(f"{command[0]} exited with status {code}")

    return produce


def table_exists(cr, name: str) -> bool:
    cr.execute("SELECT to_regclass(%s) IS NOT NULL", (f"public.{name}",))
    return cr.fetchone()[0]


def database_facts(cr) -> dict:
    """Read the facts of the manifest. `cr` sees the snapshot of the dump."""
    cr.execute("SELECT pg_database_size(current_database()), current_setting('server_version')")
    size, server_version = cr.fetchone()
    cr.execute("SELECT name, latest_version FROM ir_module_module WHERE state = 'installed' ORDER BY name")
    modules = dict(cr.fetchall())
    checksums = None
    if table_exists(cr, "ir_config_parameter"):
        cr.execute("SELECT value FROM ir_config_parameter WHERE key = %s", (CHECKSUMS_PARAM,))
        row = cr.fetchone()
        checksums = json.loads(row[0]) if row and row[0] else None
    cr.execute(
        """
        SELECT count(*),
               count(*) FILTER (WHERE store_fname LIKE '%%://%%'),
               count(*) FILTER (WHERE COALESCE(store_fname, '') != '' AND store_fname NOT LIKE '%%://%%'),
               count(*) FILTER (WHERE COALESCE(store_fname, '') = '' AND db_datas IS NOT NULL),
               count(DISTINCT store_fname) FILTER (WHERE store_fname LIKE '%%://%%')
          FROM ir_attachment
        """
    )
    total, in_storage, local, in_database, objects = cr.fetchone()
    return {
        "database_size_bytes": size,
        "pg_server_version": server_version,
        "base_version": modules.get("base"),
        "modules": modules,
        "module_checksums": checksums,
        "attachments": {
            "total": total,
            "in_storage": in_storage,
            "local": local,
            "in_database": in_database,
            "objects": objects,
        },
    }


def storage_references(cr) -> tuple[dict, dict, list]:
    """Return the fs.storage rows by code, the referenced paths by code, and the local files."""
    storages = {}
    if table_exists(cr, "fs_storage"):
        cr.execute("SELECT code, protocol, directory_path, options FROM fs_storage")
        for code, protocol, directory_path, options in cr.fetchall():
            storages[code] = {
                "protocol": protocol,
                "root": (directory_path or "").rstrip("/"),
                "options": json.loads(options) if options else {},
            }
    cr.execute(
        "SELECT DISTINCT store_fname FROM ir_attachment WHERE store_fname LIKE '%%://%%' ORDER BY store_fname"
    )
    references: dict[str, list[str]] = {}
    for (store_fname,) in cr.fetchall():
        code, path = store_fname.split("://", 1)
        references.setdefault(code, []).append(path.lstrip("/"))
    cr.execute(
        "SELECT DISTINCT store_fname FROM ir_attachment"
        " WHERE COALESCE(store_fname, '') != '' AND store_fname NOT LIKE '%%://%%' ORDER BY store_fname"
    )
    local = [row[0] for row in cr.fetchall()]
    return storages, references, local


# ---------------------------------------------------------------- filestore


def live_version(versions: list[dict], snapshot: dt.datetime) -> dict | None:
    """Return the version of an object that was live at `snapshot`."""
    live = [
        info for info in versions
        if parse_time(info.get("timeCreated")) <= snapshot
        and (parse_time(info.get("timeDeleted")) is None or parse_time(info.get("timeDeleted")) > snapshot)
    ]
    if live:
        return max(live, key=lambda info: int(info["generation"]))
    late = [
        info for info in versions
        if parse_time(info.get("timeCreated")) <= snapshot + CLOCK_SKEW
        and (parse_time(info.get("timeDeleted")) is None or parse_time(info.get("timeDeleted")) > snapshot)
    ]
    return min(late, key=lambda info: int(info["generation"])) if late else None


def storage_listing(protocol: str, root: str, options: dict, versions: bool) -> dict[str, list[dict]]:
    """List a storage root once. Return the versions of each object by full path."""
    import fsspec

    kwargs = dict(options)
    if protocol == "gcs":
        # NOTE: a separate instance, because a versioned listing fills the
        # directory cache with one entry for each generation.
        kwargs.update({"version_aware": True, "skip_instance_cache": True})
    fs = fsspec.filesystem(protocol, **kwargs)
    root = fs._strip_protocol(root).rstrip("/")
    if not root:
        return {}
    found = fs.find(root, detail=True, versions=versions) if protocol == "gcs" else fs.find(root, detail=True)
    listing: dict[str, list[dict]] = {}
    for info in found.values():
        listing.setdefault(fs._strip_protocol(info["name"]).split("#", 1)[0], []).append(info)
    return listing


def object_listing(storages: dict, references: dict, snapshot: dt.datetime) -> tuple[list, list, dict]:
    """Return the objects live at the snapshot, the missing ones, and the storages."""
    objects, missing, described = [], [], {}
    for code, paths in sorted(references.items()):
        storage = storages.get(code)
        if storage is None:
            raise BackupError(f"attachments refer to the fs.storage code {code!r}, which does not exist")
        protocol = storage["protocol"]
        listing = storage_listing(protocol, storage["root"], storage["options"], versions=True)
        root = storage["root"].strip("/") if protocol == "gcs" else storage["root"].rstrip("/")
        described[code] = {"protocol": protocol, "root": root}
        for path in paths:
            full = f"{root}/{path}" if root else path
            versions = listing.get(full, [])
            info = live_version(versions, snapshot) if protocol == "gcs" else (versions[0] if versions else None)
            if info is None:
                missing.append({"code": code, "path": path, "object": full})
                continue
            if protocol == "gcs" and not info.get("generation"):
                raise BackupError(f"GCS returned no generation for {full}")
            objects.append({
                "code": code,
                "path": path,
                "object": full,
                "size": info.get("size"),
                "generation": info.get("generation"),
                "md5": info.get("md5Hash"),
                "crc32c": info.get("crc32c"),
            })
    return objects, missing, described


def archive_producer(source: Path, names: list[str] | None, found: list):
    """Return a `produce` function that writes a tar.gz of `source`.

    `names` limits the archive to these relative paths; `found` collects the
    paths that the archive holds.
    """

    def produce(stream) -> None:
        with tarfile.open(fileobj=stream, mode="w|gz") as tar:
            if not source.is_dir():
                log(f"no local filestore at {source}")
                return
            tar.add(source, arcname="filestore", recursive=False)
            paths = sorted(source.rglob("*")) if names is None else [source / name for name in names]
            for path in paths:
                if names is not None and not path.is_file():
                    continue
                tar.add(path, arcname=f"filestore/{path.relative_to(source)}", recursive=False)
                if path.is_file():
                    found.append(str(path.relative_to(source)))

    return produce


def extract_filestore(archive: Path, dest: Path) -> int:
    """Unpack `filestore/` of an archive into the new directory `dest`."""
    dest.mkdir(parents=True, exist_ok=True)
    count = 0
    with tarfile.open(archive, "r:*") as tar:
        for member in tar.getmembers():
            name = PurePosixPath(member.name)
            if not name.parts or name.parts[0] != "filestore" or not member.isfile():
                continue
            member.name = str(PurePosixPath(*name.parts[1:]))
            tar.extract(member, dest, filter="data")
            count += 1
    return count


def source_versions(listing: dict) -> dict[str, dict[str, list[dict]]]:
    """List each source storage root of a set once."""
    return {
        code: storage_listing(storage["protocol"], storage["root"], {}, versions=True)
        for code, storage in listing["storages"].items()
    }


def missing_sources(listing: dict, sources: dict) -> list[dict]:
    """Return the listed object generations that the source storages lack."""
    missing = []
    for entry in listing["objects"]:
        versions = sources[entry["code"]].get(entry["object"], [])
        generation = entry.get("generation")
        if not versions or (generation and all(str(v.get("generation")) != str(generation) for v in versions)):
            missing.append({"object": entry["object"], "generation": generation})
    return missing


def copy_objects(listing: dict, roots: dict[str, tuple[str, str]]) -> dict:
    """Copy each listed object generation under the storage root of its code. Add only."""
    import fsspec

    copied, present = 0, 0
    jobs = []
    destinations = {}
    for code in listing["storages"]:
        protocol, root = roots[code]
        dest_fs = fsspec.filesystem(protocol, skip_instance_cache=True)
        existing = {dest_fs._strip_protocol(name) for name in dest_fs.find(root)} if root else set()
        destinations[code] = (protocol, root, dest_fs, existing)
    sources: dict[str, object] = {}
    for entry in listing["objects"]:
        source_protocol = listing["storages"][entry["code"]]["protocol"]
        if source_protocol not in sources:
            # NOTE: version_aware lets gcsfs read `object#generation`, the
            # generation that the set recorded, even after Odoo replaced or
            # deleted the live object. The storage options are not in the
            # set, because they can hold a key; the restore uses the default
            # credentials of the pod.
            kwargs = {"version_aware": True} if source_protocol == "gcs" else {}
            sources[source_protocol] = fsspec.filesystem(source_protocol, **kwargs)
        protocol, root, dest_fs, existing = destinations[entry["code"]]
        target = f"{root}/{entry['path']}"
        if target in existing:
            present += 1
            continue
        source = entry["object"]
        if entry.get("generation") and source_protocol == "gcs":
            source = f"{source}#{entry['generation']}"
        jobs.append((sources[source_protocol], source, source_protocol, dest_fs, target, protocol))

    def copy(job) -> None:
        source_fs, source, source_protocol, dest_fs, target, protocol = job
        if source_protocol == protocol == "gcs":
            source_fs.cp_file(source, target)
            return
        dest_fs.makedirs(str(PurePosixPath(target).parent), exist_ok=True)
        with source_fs.open(source, "rb") as reader, dest_fs.open(target, "wb") as writer:
            shutil.copyfileobj(reader, writer, 1 << 20)

    with ThreadPoolExecutor(max_workers=COPY_WORKERS) as pool:
        for _ in pool.map(copy, jobs):
            copied += 1
    return {
        "copied": copied,
        "already_present": present,
        "destinations": {code: f"{protocol}://{root}" for code, (protocol, root, _, _) in destinations.items()},
    }


# ---------------------------------------------------------------- backup


def image_identity() -> dict:
    return {
        "code_id": os.environ.get("FIRESTREAM_ODOO_CODE_ID") or None,
        "image": os.environ.get("FIRESTREAM_ODOO_IMAGE") or None,
        "image_digest": os.environ.get("FIRESTREAM_ODOO_IMAGE_DIGEST") or None,
    }


def backup(args) -> dict:
    if args.tag not in TAGS:
        raise BackupError(f"--tag must be one of {', '.join(TAGS)}")
    if args.filestore_mode not in FILESTORE_MODES:
        raise BackupError(f"the filestore mode must be one of {', '.join(FILESTORE_MODES)}")
    if not args.environment:
        raise BackupError("no environment name; set FIRESTREAM_BACKUP_ENV or --env")
    db = env_value("ODOO_DATABASE_NAME")
    if not db:
        raise BackupError("ODOO_DATABASE_NAME is not set")

    started = utc_now()
    timestamp = started.strftime(TIMESTAMP_FORMAT)
    set_store = open_store(base_uri(args.target, args.bucket, args.prefix)).child(
        f"{args.environment}/{timestamp}"
    )
    log(f"backup set {set_store.uri} of database {db} ({args.tag}, {args.filestore_mode})")

    files: dict[str, dict] = {}
    conn = connect(db)
    try:
        conn.set_session(isolation_level="REPEATABLE READ", readonly=True)
        with conn.cursor() as cr:
            cr.execute("SELECT pg_export_snapshot(), now()")
            snapshot, snapshot_at = cr.fetchone()
            # NOTE: pg_dump reads the snapshot of this open transaction, and
            # the queries below run in it too. The object list then names
            # exactly the attachments of the dump, even while users write.
            # The dump streams into the upload, so the pod needs no disk for it.
            files[DB_DUMP] = set_store.put(DB_DUMP, stream_command(
                ["pg_dump", "--format=custom", "--no-owner", "--no-privileges",
                 "--no-password", f"--snapshot={snapshot}", db],
                db,
            ))
            facts = database_facts(cr)
            storages, references, local_refs = storage_references(cr)
    finally:
        conn.close()
    attachments = facts["attachments"]

    missing: list[dict] = []
    local_filestore = Path(env_value("ODOO_DATA_DIR", "/firestream/odoo/data")) / "filestore" / db
    if args.filestore_mode == "archive" or local_refs:
        archived: list[str] = []
        names = None if args.filestore_mode == "archive" else local_refs
        files[FILESTORE_ARCHIVE] = set_store.put(
            FILESTORE_ARCHIVE, archive_producer(local_filestore, names, archived)
        )
        files[FILESTORE_ARCHIVE]["files"] = len(archived)
        if names is not None:
            missing += [{"code": None, "path": name, "object": str(local_filestore / name)}
                        for name in sorted(set(names) - set(archived))]

    objects_written = 0
    if args.filestore_mode == "objectManifest":
        objects, missing_objects, described = object_listing(
            storages, references, snapshot_at.astimezone(dt.timezone.utc)
        )
        missing += missing_objects
        if len(objects) + len(missing_objects) != attachments["objects"]:
            raise BackupError(
                f"the object list has {len(objects) + len(missing_objects)} entries, "
                f"ir_attachment refers to {attachments['objects']}"
            )
        listing = json.dumps({
            "format": SET_FORMAT,
            "snapshot_at": iso(snapshot_at.astimezone(dt.timezone.utc)),
            "storages": described,
            "count": len(objects),
            "objects": objects,
            "missing": missing,
        }, indent=1, sort_keys=True).encode()
        files[FILESTORE_OBJECTS] = {
            **set_store.put_bytes(FILESTORE_OBJECTS, listing),
            "objects": len(objects),
            "missing": len(missing),
        }
        objects_written = len(objects)
    if missing and not args.allow_missing_objects:
        sample = ", ".join(entry["object"] for entry in missing[:5])
        raise BackupError(f"{len(missing)} referenced objects are not in their storage: {sample}")

    finished = utc_now()
    manifest = {
        "format": SET_FORMAT,
        "tag": args.tag,
        "environment": args.environment,
        "database": db,
        "odoo_version": env_value("ODOO_VERSION") or None,
        **image_identity(),
        **facts,
        "filestore_mode": args.filestore_mode,
        "object_count": objects_written,
        "missing_count": len(missing),
        "files": files,
        "snapshot_at": iso(snapshot_at.astimezone(dt.timezone.utc)),
        "started_at": iso(started),
        "finished_at": iso(finished),
    }
    set_store.put_bytes(MANIFEST, json.dumps(manifest, indent=1, sort_keys=True).encode())

    log(f"backup complete: {set_store.uri}")
    return {
        "set": set_store.uri,
        "tag": args.tag,
        "environment": args.environment,
        "database": db,
        "filestore_mode": args.filestore_mode,
        "db_dump_sha256": files[DB_DUMP]["sha256"],
        "db_dump_bytes": files[DB_DUMP]["size"],
        "object_count": objects_written,
        "missing_count": len(missing),
        "attachments": facts["attachments"],
        "started_at": manifest["started_at"],
        "finished_at": manifest["finished_at"],
    }


# ---------------------------------------------------------------- restore


def admin(maintenance_db: str, statement, params=None, fetch=False):
    conn = connect(maintenance_db)
    try:
        conn.autocommit = True
        with conn.cursor() as cr:
            cr.execute(statement, params)
            return cr.fetchall() if fetch else None
    finally:
        conn.close()


def database_exists(maintenance_db: str, name: str) -> bool:
    return bool(admin(maintenance_db, "SELECT 1 FROM pg_database WHERE datname = %s", (name,), fetch=True))


def identifier(name: str):
    from psycopg2 import sql

    return sql.Identifier(name)


def create_database(maintenance_db: str, name: str) -> None:
    from psycopg2 import sql

    # NOTE: the same encoding, collation and template as Odoo's own database
    # create, so the restored database sorts and compares as Odoo expects.
    admin(maintenance_db, sql.SQL(
        "CREATE DATABASE {} ENCODING 'unicode' LC_COLLATE 'C' TEMPLATE template0"
    ).format(identifier(name)))


def drop_database(maintenance_db: str, name: str) -> None:
    from psycopg2 import sql

    admin(maintenance_db, sql.SQL("DROP DATABASE IF EXISTS {}").format(identifier(name)))


def session_count(maintenance_db: str, name: str) -> int:
    return admin(
        maintenance_db,
        "SELECT count(*) FROM pg_stat_activity WHERE datname = %s AND pid <> pg_backend_pid()",
        (name,), fetch=True,
    )[0][0]


def terminate_sessions(maintenance_db: str, name: str) -> None:
    admin(
        maintenance_db,
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = %s AND pid <> pg_backend_pid()",
        (name,),
    )


def set_allow_connections(maintenance_db: str, name: str, allow: bool) -> None:
    from psycopg2 import sql

    admin(maintenance_db, sql.SQL("ALTER DATABASE {} ALLOW_CONNECTIONS {}").format(
        identifier(name), sql.SQL("true" if allow else "false")))


def close_database(maintenance_db: str, name: str, attempts: int = 5) -> None:
    """Refuse new sessions on `name` and end the open ones. Raise when some stay."""
    set_allow_connections(maintenance_db, name, False)
    for _ in range(attempts):
        terminate_sessions(maintenance_db, name)
        time.sleep(0.5)
        if session_count(maintenance_db, name) == 0:
            return
    set_allow_connections(maintenance_db, name, True)
    raise BackupError(f"clients keep a session on {name!r}; stop them and run the restore again")


def rename_databases(maintenance_db: str, renames: list[tuple[str, str]]) -> None:
    """Rename the databases in one transaction: all of them or none."""
    from psycopg2 import sql

    conn = connect(maintenance_db)
    try:
        with conn.cursor() as cr:
            for old, new in renames:
                cr.execute(sql.SQL("ALTER DATABASE {} RENAME TO {}").format(identifier(old), identifier(new)))
        conn.commit()
    finally:
        conn.close()


def side_name(target: str, label: str, stamp: str) -> str:
    suffix = f"_{label}_{stamp}"
    return f"{target[:63 - len(suffix)]}{suffix}"


def parse_storage_roots(values: list[str]) -> dict[str, tuple[str, str]]:
    roots = {}
    for value in values or []:
        code, sep, uri = value.partition("=")
        if not sep or not code or "://" not in uri:
            raise BackupError(f"--storage-root takes CODE=URI, for example platform_filestore=gs://bucket/dir; got {value!r}")
        roots[code] = split_uri(uri)
    return roots


def rewrite_storages(db: str, roots: dict[str, tuple[str, str]], keep: bool) -> dict:
    """Point the fs.storage rows of the restored database at the target roots."""
    conn = connect(db)
    try:
        with conn.cursor() as cr:
            if not table_exists(cr, "fs_storage"):
                if roots:
                    raise BackupError("--storage-root was given, but the database has no fs_storage table")
                return {}
            cr.execute("SELECT code FROM fs_storage ORDER BY code")
            codes = [row[0] for row in cr.fetchall()]
            unknown = sorted(set(roots) - set(codes))
            if unknown:
                raise BackupError(f"the database has no fs.storage with the code {', '.join(unknown)}")
            unset = [code for code in codes if code not in roots]
            if unset and not keep:
                raise BackupError(
                    f"the database keeps the fs.storage roots of the source for {', '.join(unset)}. "
                    "Give --storage-root CODE=URI for each, or --keep-storage-roots to keep them"
                )
            for code, (protocol, root) in roots.items():
                cr.execute("UPDATE fs_storage SET protocol = %s, directory_path = %s WHERE code = %s",
                           (protocol, root, code))
        conn.commit()
    finally:
        conn.close()
    return {code: f"{protocol}://{root}" for code, (protocol, root) in roots.items()}


def load_set(args, work: Path) -> tuple[dict, Path, Path | None, dict | None, str]:
    """Fetch a set or unpack an archive. Return manifest, dump, filestore archive, listing, source."""
    if args.archive:
        with tarfile.open(args.archive, "r:*") as tar:
            names = set(tar.getnames())
            for member in (MANIFEST, DB_DUMP, "dump.sql"):
                if member in names:
                    tar.extract(member, work, filter="data")
        if not (work / MANIFEST).exists():
            raise BackupError(f"{args.archive} has no {MANIFEST}; it is not a Firestream Odoo archive")
        manifest = json.loads((work / MANIFEST).read_text())
        manifest.setdefault("odoo_version", manifest.get("version"))
        dump = work / DB_DUMP if (work / DB_DUMP).exists() else work / "dump.sql"
        if not dump.exists():
            raise BackupError(f"{args.archive} has neither {DB_DUMP} nor dump.sql")
        return manifest, dump, Path(args.archive), None, str(args.archive)

    store = open_store(resolve_set(args))
    manifest = store.read_json(MANIFEST)
    if manifest is None:
        raise BackupError(
            f"{store.uri} has no {MANIFEST}: the set is incomplete (the backup did not finish), "
            "and a restore refuses it"
        )
    if manifest.get("format") != SET_FORMAT:
        raise BackupError(f"{store.uri}: unknown set format {manifest.get('format')!r}")
    files = manifest.get("files", {})
    listing = None
    if FILESTORE_OBJECTS in files:
        data = store.read_bytes(FILESTORE_OBJECTS) or b""
        if hashlib.sha256(data).hexdigest() != files[FILESTORE_OBJECTS]["sha256"]:
            raise BackupError(f"{FILESTORE_OBJECTS} does not match the sha256 of the manifest")
        listing = json.loads(data)
    dump = work / DB_DUMP
    log(f"download {store.uri}/{DB_DUMP}")
    store.get_file(DB_DUMP, dump)
    digest = file_digests(dump)["sha256"]
    if digest != files.get(DB_DUMP, {}).get("sha256"):
        raise BackupError(f"{DB_DUMP} has sha256 {digest}, the manifest says {files.get(DB_DUMP, {}).get('sha256')}")
    archive = None
    if FILESTORE_ARCHIVE in files:
        archive = work / FILESTORE_ARCHIVE
        store.get_file(FILESTORE_ARCHIVE, archive)
        if file_digests(archive)["sha256"] != files[FILESTORE_ARCHIVE]["sha256"]:
            raise BackupError(f"{FILESTORE_ARCHIVE} does not match the sha256 of the manifest")
    return manifest, dump, archive, listing, store.uri


def check_version(manifest: dict, force: bool) -> None:
    image_version = env_value("ODOO_VERSION")
    set_version = manifest.get("odoo_version") or manifest.get("base_version")
    if not (set_version and image_version) or major(set_version) == major(image_version):
        return
    if not force:
        raise BackupError(
            f"the set is Odoo {set_version} and this image is Odoo {image_version}; use --force to restore anyway"
        )
    log(f"the set is Odoo {set_version}, the image is Odoo {image_version}; --force given")


def object_roots(listing: dict | None, roots: dict, filestore_bucket: str | None) -> dict | None:
    """Return the copy destination of each storage code, or None for no copy."""
    if not filestore_bucket:
        return None
    if listing is None:
        raise BackupError("--filestore-bucket needs a set in objectManifest mode")
    wanted = split_uri(filestore_bucket)
    for code in listing["storages"]:
        if roots.get(code) != wanted:
            raise BackupError(
                f"--filestore-bucket {filestore_bucket} needs --storage-root {code}={filestore_bucket}, "
                "so that the restored database reads the objects where the restore copies them"
            )
    return {code: roots[code] for code in listing["storages"]}


def restore(args, neutralize=None) -> dict:
    """Restore into a temporary database, prepare it there, then give it the target name.

    No client sees the new database before its storage roots point at the
    target, and before neutralize ran. The target keeps its content until
    the rename, so a failure at any step leaves it as it was.
    """
    if bool(args.set) == bool(args.archive):
        raise BackupError("give exactly one of --set and --archive")
    if args.neutralize and neutralize is None:
        raise BackupError("--neutralize needs Odoo on the Python path")
    roots = parse_storage_roots(args.storage_root)
    target = args.database
    maintenance = args.maintenance_db
    stamp = utc_now().strftime(TIMESTAMP_FORMAT).lower()
    temp = side_name(target, "restore", stamp)
    before = side_name(target, "before", stamp)
    filestores = Path(env_value("ODOO_DATA_DIR", "/firestream/odoo/data")) / "filestore"
    report: dict = {"database": target}

    with tempfile.TemporaryDirectory(prefix="firestream-odoo-restore-") as tmp:
        work = Path(tmp)
        manifest, dump, archive, listing, source = load_set(args, work)
        report.update({"source": source, "source_database": manifest.get("database")})
        check_version(manifest, args.force)
        copy_roots = object_roots(listing, roots, args.filestore_bucket)
        if copy_roots is not None:
            missing = missing_sources(listing, source_versions(listing))
            if missing:
                sample = ", ".join(f"{entry['object']}#{entry['generation']}" for entry in missing[:5])
                raise BackupError(
                    f"{len(missing)} listed object generations are not in the source storage "
                    f"(nothing was restored): {sample}"
                )

        exists = database_exists(maintenance, target)
        if exists and not args.replace:
            raise BackupError(
                f"the database {target!r} exists. Restore into a new name, or give --replace "
                "to swap it with the restored copy"
            )

        create_database(maintenance, temp)
        try:
            if dump.name == "dump.sql":
                # NOTE: an archive older than the custom format. Its plain
                # dump drops each object first, and psql goes on past the
                # statements that a database owner may not run (DROP
                # EXTENSION), as the restore of that format always did.
                run(["psql", "--no-password", "--quiet", f"--file={dump}", temp], temp)
            else:
                # NOTE: not --single-transaction. One transaction holds a lock
                # on every table, index and sequence it creates, and a full
                # Odoo database exceeds max_locks_per_transaction. The target
                # is a new database that no client knows, so a failure only
                # drops it.
                run(["pg_restore", "--no-owner", "--no-privileges", "--no-password", "--exit-on-error",
                     f"--jobs={args.jobs}", f"--dbname={temp}", str(dump)], temp)

            conn = connect(temp)
            try:
                with conn.cursor() as cr:
                    cr.execute("SELECT count(*) FROM ir_attachment")
                    restored_attachments = cr.fetchone()[0]
            finally:
                conn.close()
            expected = (manifest.get("attachments") or {}).get("total")
            if expected is not None and restored_attachments != expected:
                raise BackupError(
                    f"the restored database has {restored_attachments} attachments, the manifest says {expected}"
                )
            report["attachments"] = restored_attachments
            report["storage_roots"] = rewrite_storages(temp, roots, args.keep_storage_roots)
            if args.neutralize:
                neutralize(temp)
            if archive is not None:
                report["filestore_files"] = extract_filestore(archive, filestores / temp)
            if listing is not None:
                report["objects"] = (copy_objects(listing, copy_roots) if copy_roots is not None
                                     else {"skipped": True, "count": listing["count"]})

            terminate_sessions(maintenance, temp)
            if exists:
                close_database(maintenance, target)
                try:
                    rename_databases(maintenance, [(target, before), (temp, target)])
                except Exception:
                    set_allow_connections(maintenance, target, True)
                    raise
                report["previous_database"] = before
            else:
                rename_databases(maintenance, [(temp, target)])
        except BaseException:
            # WARNING: drop only the temporary database of this run. The
            # target keeps its name and content until the rename above.
            drop_database(maintenance, temp)
            shutil.rmtree(filestores / temp, ignore_errors=True)
            raise

    if (filestores / temp).exists():
        if (filestores / target).exists():
            (filestores / target).rename(filestores / before)
        (filestores / temp).rename(filestores / target)
        report["filestore_dir"] = str(filestores / target)
    report["neutralized"] = bool(args.neutralize)
    report["replaced"] = exists
    report["manifest"] = {
        key: manifest.get(key)
        for key in ("tag", "environment", "odoo_version", "image", "image_digest", "started_at", "finished_at")
    }
    log(f"restore of {target} from {report['source']} complete")
    return report


# ---------------------------------------------------------------- list


def list_sets(args) -> dict:
    root = open_store(base_uri(args.target, args.bucket, args.prefix))
    environments = [args.environment] if args.environment else root.list_dirs()
    sets = []
    for environment in environments:
        env_store = root.child(environment)
        for timestamp in env_store.list_dirs():
            set_store = env_store.child(timestamp)
            manifest = set_store.read_json(MANIFEST)
            entry = {"set": set_store.uri, "environment": environment, "timestamp": timestamp,
                     "complete": bool(manifest) and manifest.get("format") == SET_FORMAT}
            if manifest:
                entry.update({
                    "tag": manifest.get("tag"),
                    "database": manifest.get("database"),
                    "odoo_version": manifest.get("odoo_version"),
                    "image": manifest.get("image"),
                    "image_digest": manifest.get("image_digest"),
                    "filestore_mode": manifest.get("filestore_mode"),
                    "object_count": manifest.get("object_count"),
                    "database_size_bytes": manifest.get("database_size_bytes"),
                    "db_dump_sha256": manifest.get("files", {}).get(DB_DUMP, {}).get("sha256"),
                    "finished_at": manifest.get("finished_at"),
                })
            sets.append(entry)
    if getattr(args, "latest", False):
        return {"root": root.uri, "latest": latest_entry(sets, root.uri)}
    return {"root": root.uri, "sets": sets, "count": len(sets)}


def latest_entry(sets: list[dict], root: str) -> dict:
    """Return the newest complete set. A set without a manifest never counts."""
    complete = [entry for entry in sets if entry["complete"]]
    if not complete:
        raise BackupError(f"no complete backup set under {root}")
    return max(complete, key=lambda entry: (entry["timestamp"], entry["environment"]))


def resolve_set(args) -> str:
    """Return the URI of `--set`; `latest` is the newest complete set of `--env`."""
    if args.set != "latest":
        return args.set
    if not args.environment:
        raise BackupError("--set latest needs the environment; set FIRESTREAM_BACKUP_ENV or --env")
    args.latest = True
    return list_sets(args)["latest"]["set"]


# ---------------------------------------------------------------- arguments


def add_location_arguments(parser) -> None:
    parser.add_argument("--target", choices=TARGETS, default=env_value("FIRESTREAM_BACKUP_TARGET", "gcs"),
                        help="the object store (default: $FIRESTREAM_BACKUP_TARGET or gcs)")
    parser.add_argument("--bucket", default=env_value("FIRESTREAM_BACKUP_BUCKET"),
                        help="the bucket; an absolute directory for the file target")
    parser.add_argument("--prefix", default=env_value("FIRESTREAM_BACKUP_PREFIX", "odoo-backups"),
                        help="the prefix in the bucket")
    parser.add_argument("--env", dest="environment", default=env_value("FIRESTREAM_BACKUP_ENV"),
                        help="the environment name (default: $FIRESTREAM_BACKUP_ENV)")


def add_backup_arguments(parser) -> None:
    add_location_arguments(parser)
    parser.add_argument("--tag", default=env_value("FIRESTREAM_BACKUP_TAG", "manual"),
                        help=f"one of {', '.join(TAGS)} (default: manual)")
    parser.add_argument("--filestore-mode", choices=FILESTORE_MODES,
                        default=env_value("FIRESTREAM_BACKUP_FILESTORE_MODE", "archive"))
    parser.add_argument("--allow-missing-objects", action="store_true",
                        help="finish the set when an attachment file is not in its storage")


def add_restore_arguments(parser) -> None:
    parser.add_argument("--set", help="the set URI, for example gs://bucket/odoo-backups/production/20261014T010000Z, "
                                      "or `latest`: the newest complete set of --env")
    parser.add_argument("--archive", help="a local archive of odoo_dump (instead of --set)")
    parser.add_argument("--database", required=True, help="the database to create from the set")
    parser.add_argument("--storage-root", action="append", default=[], metavar="CODE=URI",
                        help="point the fs.storage CODE of the restored database at URI, "
                             "for example platform_filestore=gs://bucket/dir (repeatable)")
    parser.add_argument("--keep-storage-roots", action="store_true",
                        help="keep the fs.storage roots of the source (a restore into the same environment)")
    parser.add_argument("--filestore-bucket",
                        help="copy the listed objects under this URI (add only); "
                             "it must equal the --storage-root of each storage of the set")
    parser.add_argument("--neutralize", action="store_true",
                        help="run Odoo's neutralize on the restored database before it gets its name")
    parser.add_argument("--replace", action="store_true",
                        help="swap an existing database with the restored copy; the old one stays "
                             "as <database>_before_<timestamp>")
    parser.add_argument("--force", action="store_true", help="restore a set of another Odoo major version")
    parser.add_argument("--jobs", type=int, default=4, help="parallel pg_restore jobs (default: 4)")
    parser.add_argument("--maintenance-db", default=env_value("ODOO_DATABASE_MAINTENANCE", "postgres"),
                        help="the database to connect to for CREATE DATABASE (default: postgres)")
