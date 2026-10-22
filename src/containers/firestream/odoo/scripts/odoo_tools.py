"""Release commands of the Firestream Odoo image.

`firestream-odoo-sync` brings the database in step with the modules of the
image. It installs each module of ODOO_INSTALL_MODULES that the database does
not have, and it updates only the modules whose files changed since the last
sync. The checksums are those of OCA `module_auto_update`, and the sync saves
them with that module after a successful update, as its
`upgrade_changed_checksum()` does. It prints one JSON report line on stdout.
The Odoo log goes to stderr.

`firestream-odoo-call` calls one model method as the superuser and prints its
result as one JSON line. A release uses it for one-time data moves.

`firestream-odoo-backup`, `firestream-odoo-restore` and `firestream-odoo-backups`
write, restore and list backup sets (see `odoo_backup.py`). They read the
ODOO_DATABASE_* variables.

The sync and the call read the database settings from the Odoo config file of
the container. In a Job or a shell without that file, they read the ODOO_*
variables.
"""

from __future__ import annotations

import argparse
import importlib.util
import inspect
import json
import os
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

import odoo_backup
from odoo_backup import env_value

CHECKSUMS_PARAM = "module_auto_update.installed_checksums"
EXCLUDE_PARAM = "module_auto_update.exclude_patterns"
DEFAULT_EXCLUDE = "*.pyc,*.pyo,i18n/*.pot,i18n_extra/*.pot,static/*,tests/*"
PARTIAL_STATES = ("to install", "to upgrade", "to remove")
# NOTE: a fixed key, so that two syncs on one database exclude each other
# from any pod. The value is arbitrary but must never change.
ADVISORY_LOCK_KEY = 0x46534F44


class SyncError(RuntimeError):
    """The sync cannot go on; the message tells what to do."""


def database_name() -> str:
    name = env_value("ODOO_DATABASE_NAME")
    if not name:
        raise SyncError("ODOO_DATABASE_NAME is not set")
    return name


def conf_from_env(directory: Path) -> Path:
    """Write an Odoo config file from the ODOO_* variables; return its path."""
    addons_dir = env_value("ODOO_ADDONS_DIR", "/opt/firestream/odoo/addons")
    addons_path = env_value("ODOO_ADDONS_PATH").replace("{{ODOO_ADDONS_DIR}}", addons_dir)
    if not addons_path:
        raise SyncError("ODOO_ADDONS_PATH is not set and no Odoo config file exists")
    lines = [
        "[options]",
        f"addons_path = {addons_path}",
        f"data_dir = {env_value('ODOO_DATA_DIR', '/firestream/odoo/data')}",
        f"db_host = {env_value('ODOO_DATABASE_HOST', 'postgresql')}",
        f"db_port = {env_value('ODOO_DATABASE_PORT_NUMBER', '5432')}",
        f"db_user = {env_value('ODOO_DATABASE_USER', 'firestream')}",
        f"db_password = {env_value('ODOO_DATABASE_PASSWORD')}",
        "list_db = False",
    ]
    path = directory / "odoo.conf"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write("\n".join(lines) + "\n")
    return path


@contextmanager
def odoo_config(db: str | None = None):
    """Load the Odoo config of the container; yield the database name."""
    from odoo.tools import config

    db = db or database_name()
    with tempfile.TemporaryDirectory(prefix="firestream-odoo-") as tmp:
        conf = Path(env_value("ODOO_CONF_FILE", "/opt/firestream/odoo/conf/odoo.conf"))
        if not conf.is_file():
            conf = conf_from_env(Path(tmp))
        config.parse_config(
            ["-c", str(conf), "-d", db, "--logfile=", "--no-http"], setup_logging=True
        )
        yield db


def load_addon_hash():
    """The `addon_hash` function of module_auto_update, or None when it is not on the path."""
    from odoo.modules.module import get_module_path

    path = get_module_path("module_auto_update", display_warning=False)
    if not path:
        return None
    spec = importlib.util.spec_from_file_location(
        "firestream_addon_hash", Path(path) / "addon_hash.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.addon_hash


def read_plan(cr, wanted: list[str]) -> dict:
    """What a sync would do, from SQL and the files only. It writes nothing."""
    from odoo.modules.module import get_module_path

    cr.execute("SELECT to_regclass('public.ir_module_module') IS NOT NULL")
    if not cr.fetchone()[0]:
        raise SyncError("the database has no Odoo tables; the first boot initializes it")
    cr.execute("SELECT name, state FROM ir_module_module")
    states = dict(cr.fetchall())
    # NOTE: base in `to upgrade` or `to remove` is a change that did not
    # finish; the sync's update load finishes it. Any other state means that
    # the first init never ran.
    if states.get("base") not in ("installed", "to upgrade", "to remove"):
        raise SyncError("the base module is not installed; the first boot initializes it")

    cr.execute(
        "SELECT key, value FROM ir_config_parameter WHERE key IN %s",
        ((CHECKSUMS_PARAM, EXCLUDE_PARAM),),
    )
    params = dict(cr.fetchall())
    cr.execute("SELECT code FROM res_lang WHERE active")
    languages = [row[0] for row in cr.fetchall()]

    installed = sorted(name for name, state in states.items() if state == "installed")
    saved = params.get(CHECKSUMS_PARAM)
    baseline = not saved
    if baseline:
        changed = installed
    else:
        addon_hash = load_addon_hash()
        if addon_hash is None:
            raise SyncError("module_auto_update is not on the addons path")
        checksums = json.loads(saved)
        patterns = [p.strip() for p in params.get(EXCLUDE_PARAM, DEFAULT_EXCLUDE).split(",")]
        changed = []
        for name in installed:
            path = get_module_path(name, display_warning=False)
            current = addon_hash(path, patterns, languages) if path else False
            if current != checksums.get(name):
                changed.append(name)
    return {
        "install": [name for name in wanted if states.get(name) != "installed"],
        "update": changed,
        "partial": sorted(name for name, state in states.items() if state in PARTIAL_STATES),
        "baseline": baseline,
    }


@contextmanager
def sync_lock(db: str):
    """Hold a session advisory lock on the database for the whole sync."""
    from odoo.sql_db import db_connect

    with db_connect(db).cursor() as cr:
        cr.execute("SELECT pg_try_advisory_lock(%s)", (ADVISORY_LOCK_KEY,))
        if not cr.fetchone()[0]:
            raise SyncError("another firestream-odoo-sync runs on this database")
        try:
            yield
        finally:
            cr.execute("SELECT pg_advisory_unlock(%s)", (ADVISORY_LOCK_KEY,))


def load_with_update(db: str, install: list[str], update: list[str]) -> None:
    """Load the registry once, and install and update modules in that load.

    NOTE: Odoo runs every `_register_hook` at the end of a registry load. A
    load without an update would run the hooks of the new code against the
    old schema, before any update. One load with `-i`/`-u` semantics updates
    the schema first, as `odoo-bin -i ... -u ...` does.
    """
    from odoo.modules.registry import Registry
    from odoo.tools import config

    config["init"] = dict.fromkeys(install, True)
    config["update"] = dict.fromkeys(update, True)
    try:
        Registry.new(db, update_module=True)
    finally:
        config["init"] = {}
        config["update"] = {}


@contextmanager
def superuser_env(db: str):
    from odoo import SUPERUSER_ID, api
    from odoo.modules.registry import Registry

    with Registry(db).cursor() as cr:
        yield api.Environment(cr, SUPERUSER_ID, {})


def save_checksums(db: str, required: bool = True) -> bool:
    """Save the checksums of the installed modules, as `upgrade_changed_checksum` does.

    Return False when module_auto_update is not installed and `required` is false.
    """
    with superuser_env(db) as env:
        model = env["ir.module.module"]
        if not hasattr(model, "_save_installed_checksums"):
            if not required:
                return False
            raise SyncError(
                "module_auto_update is not installed; add it to the install modules of the image"
            )
        model._save_installed_checksums()
        env.cr.commit()
    return True


def check_result(db: str, wanted: list[str]) -> None:
    from odoo.sql_db import db_connect

    with db_connect(db).cursor() as cr:
        cr.execute("SELECT name, state FROM ir_module_module")
        states = dict(cr.fetchall())
    missing = [name for name in wanted if states.get(name) != "installed"]
    if missing:
        raise SyncError(f"these modules are not installed after the sync: {', '.join(missing)}")
    partial = sorted(name for name, state in states.items() if state in PARTIAL_STATES)
    if partial:
        raise SyncError(f"these modules did not finish their change: {', '.join(partial)}")


def sync(args: argparse.Namespace) -> dict:
    from odoo.sql_db import db_connect

    wanted = [m.strip() for m in env_value("ODOO_INSTALL_MODULES").split(",") if m.strip()]
    with odoo_config() as db:
        if args.save_checksums:
            with sync_lock(db):
                return {"database": db, "checksums_saved": save_checksums(db, required=False)}
        with db_connect(db).cursor() as cr:
            plan = read_plan(cr, wanted)
            cr.rollback()
        full = plan["baseline"] or args.baseline
        report = {
            "database": db,
            "installed": plan["install"],
            "updated": ["all"] if full else plan["update"],
            "baseline": full,
        }
        if args.dry_run:
            return {**report, "partial": plan["partial"]}

        # NOTE: without saved checksums, every module counts as changed. A
        # full update is then the only way to make the checksums true.
        update = ["all"] if full else plan["update"]
        with sync_lock(db):
            if plan["install"] or update or plan["partial"]:
                load_with_update(db, plan["install"], update)
                save_checksums(db)
            check_result(db, wanted)
        return report


def call_arguments(method, kwargs: dict, dry_run: bool) -> tuple[dict, bool]:
    """Return the keyword arguments of the call and whether it is a dry run.

    A dry run passes `dry_run=True` to the method. A method without a
    `dry_run` parameter cannot run as a dry run, and the call refuses it.
    """
    if kwargs.get("dry_run") is True:
        dry_run = True
    if not dry_run:
        return kwargs, False
    if "dry_run" in kwargs and kwargs["dry_run"] is not True:
        raise SyncError('--dry-run conflicts with "dry_run" in --kwargs')
    parameters = inspect.signature(method).parameters
    # NOTE: a rollback alone does not make a dry run. A method can commit
    # each batch, and it can write to an object store or send mail, which no
    # rollback undoes. Only the method can promise that it writes nothing.
    if "dry_run" not in parameters:
        raise SyncError(
            f"{getattr(method, '__name__', method)} has no dry_run parameter, so --dry-run "
            "cannot promise that it writes nothing; nothing was called"
        )
    return {**kwargs, "dry_run": True}, True


class CommitRefused(SyncError):
    """A dry run tried to commit."""


@contextmanager
def commits_refused(cursor_class):
    """Make `commit` of every cursor of `cursor_class` raise, for the block."""
    original = cursor_class.commit

    def refuse(self, *args, **kwargs):
        raise CommitRefused("the method tried to commit during --dry-run; the transaction is rolled back")

    cursor_class.commit = refuse
    try:
        yield
    finally:
        cursor_class.commit = original


def invoke(env, model: str, method_name: str, kwargs: dict, dry_run: bool, cursor_class) -> tuple:
    """Call one model method. Return the result and whether it was a dry run."""
    if model not in env:
        raise SyncError(f"the model {model!r} does not exist in this database")
    method = getattr(env[model], method_name, None)
    if not callable(method):
        raise SyncError(f"the model {model!r} has no method {method_name!r}")
    kwargs, dry_run = call_arguments(method, kwargs, dry_run)
    if not dry_run:
        result = method(**kwargs)
        env.cr.commit()
        return result, False
    # WARNING: keep the commit guard although the method got dry_run=True. A
    # method that ignores the flag would otherwise write the production
    # database during a command that promised to write nothing.
    try:
        # NOTE: a read-only transaction makes PostgreSQL refuse each write of
        # the call, even one that a method makes before its own dry_run check.
        env.cr.execute("SET TRANSACTION READ ONLY")
        with commits_refused(cursor_class):
            result = method(**kwargs)
    finally:
        env.cr.rollback()
    return result, True


def call(args: argparse.Namespace) -> dict:
    from odoo.sql_db import Cursor

    kwargs = json.loads(args.kwargs)
    if not isinstance(kwargs, dict):
        raise SyncError("--kwargs must be a JSON object")
    with odoo_config() as db, superuser_env(db) as env:
        result, args.dry_run = invoke(env, args.model, args.method, kwargs, args.dry_run, Cursor)
    return {"database": db, "model": args.model, "method": args.method, "result": result}


def neutralize(db: str) -> None:
    """Run `odoo-bin neutralize -d <db>`: no outgoing mail, no crons, no payments."""
    from odoo.modules.neutralize import neutralize_database
    from odoo.sql_db import close_db, db_connect

    with odoo_config(db):
        try:
            with db_connect(db).cursor() as cr:
                neutralize_database(cr)
        finally:
            # NOTE: the Odoo pool keeps its connection open, and the restore
            # renames this database next, which needs no session on it.
            close_db(db)


def backup(args: argparse.Namespace) -> dict:
    return odoo_backup.backup(args)


def restore(args: argparse.Namespace) -> dict:
    return odoo_backup.restore(args, neutralize=neutralize)


def backups(args: argparse.Namespace) -> dict:
    return odoo_backup.list_sets(args)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(prog="firestream-odoo")
    commands = root.add_subparsers(dest="command", required=True)

    sync_parser = commands.add_parser(
        "sync", help="install the missing modules and update the changed modules"
    )
    sync_parser.add_argument(
        "--dry-run", action="store_true", help="report what would change; write nothing"
    )
    sync_parser.add_argument(
        "--baseline",
        action="store_true",
        help="update every installed module, then save the checksums of all of them",
    )
    sync_parser.add_argument(
        "--save-checksums",
        action="store_true",
        help="save the checksums of the installed modules without any update "
        "(after a first install; a no-op without module_auto_update)",
    )
    sync_parser.set_defaults(run=sync)

    call_parser = commands.add_parser("call", help="call one model method as the superuser")
    call_parser.add_argument("model", help="the model name, for example ir.attachment")
    call_parser.add_argument("method", help="the method name")
    call_parser.add_argument("--kwargs", default="{}", help="the keyword arguments, as a JSON object")
    call_parser.add_argument(
        "--dry-run",
        action="store_true",
        help="pass dry_run=True to the method and roll back; refused for a method without dry_run",
    )
    call_parser.set_defaults(run=call)

    backup_parser = commands.add_parser("backup", help="write one backup set")
    odoo_backup.add_backup_arguments(backup_parser)
    backup_parser.set_defaults(run=backup)

    restore_parser = commands.add_parser("restore", help="restore a backup set into a database")
    odoo_backup.add_location_arguments(restore_parser)
    odoo_backup.add_restore_arguments(restore_parser)
    restore_parser.set_defaults(run=restore)

    backups_parser = commands.add_parser("backups", help="list the backup sets")
    backups_parser.add_argument("action", choices=["list"])
    backups_parser.add_argument("--latest", action="store_true", help="print only the newest complete set")
    odoo_backup.add_location_arguments(backups_parser)
    backups_parser.set_defaults(run=backups)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    started = time.monotonic()
    report: dict = {"command": args.command, "dry_run": bool(getattr(args, "dry_run", False))}
    try:
        report.update(args.run(args))
        report["dry_run"] = bool(getattr(args, "dry_run", False))
        report["status"] = "ok"
        code = 0
    except Exception as error:  # noqa: BLE001 - the report must name every failure
        report["status"] = "error"
        report["error"] = f"{type(error).__name__}: {error}"
        code = 1
    report["duration_seconds"] = round(time.monotonic() - started, 3)
    sys.stdout.write(json.dumps(report, sort_keys=True, default=str) + "\n")
    sys.stdout.flush()
    return code


if __name__ == "__main__":
    sys.exit(main())
