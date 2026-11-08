import json
import re
import subprocess
import sys
from pathlib import Path

ALLOWED = {"backport", "equivalent", "deferred", "not-applicable"}
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def fail(message: str) -> None:
    print(f"upstream sync verification failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def git(cwd: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(cwd), *args],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        fail(f"git {' '.join(args)} failed in {cwd}: {result.stderr.strip()}")
    return result.stdout


def read_audit(audit_path: Path, upstream: Path) -> dict:
    if not audit_path.is_file():
        fail(f"audit file does not exist: {audit_path}")
    if not (upstream / ".git").exists():
        fail(f"upstream repository is unavailable: {upstream}")

    with audit_path.open(encoding="utf-8") as handle:
        audit = json.load(handle)

    if audit.get("schema_version") != 1:
        fail("unsupported schema_version")
    return audit


def require_sha(label: str, value: object) -> str:
    if not isinstance(value, str) or not SHA_RE.fullmatch(value):
        fail(f"{label} must be a full 40-character commit hash")
    return value


def validate_audit(audit: dict):
    baseline = audit.get("baseline", "")
    audited = audit.get("audited_upstream", "")
    paths = audit.get("paths")
    entries = audit.get("entries")
    for label, value in (("baseline", baseline), ("audited_upstream", audited)):
        require_sha(label, value)
    if not isinstance(paths, list) or not paths or not all(isinstance(p, str) and p for p in paths):
        fail("paths must be a non-empty string array")
    if not isinstance(entries, list):
        fail("entries must be an array")

    seen = set()
    classified = []
    backports = []
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            fail(f"entry {index} is not an object")
        commit = entry.get("commit", "")
        status = entry.get("status", "")
        note = entry.get("note", "")
        if not SHA_RE.fullmatch(commit):
            fail(f"entry {index} does not use a full commit hash")
        if commit in seen:
            fail(f"duplicate entry: {commit}")
        if status not in ALLOWED:
            fail(f"invalid status for {commit}: {status}")
        if not isinstance(note, str) or not note.strip():
            fail(f"entry {commit} must explain its classification")
        seen.add(commit)
        classified.append(commit)
        if status == "backport":
            backports.append(commit)
    return baseline, audited, paths, entries, seen, classified, backports


def verify_range(upstream, baseline, audited, compared, newest_by_date, newest_by_trailer,
                 paths, classified, seen):
    for commit in (baseline, audited):
        git(upstream, "cat-file", "-e", f"{commit}^{{commit}}")
    for label, commit in (
        ("compared_against", compared),
        ("newest_ported_by_date", newest_by_date),
        ("newest_ported_by_trailer", newest_by_trailer),
    ):
        if commit:
            git(upstream, "cat-file", "-e", f"{commit}^{{commit}}")
    ancestor = subprocess.run(
        ["git", "-C", str(upstream), "merge-base", "--is-ancestor", baseline, audited]
    )
    if ancestor.returncode != 0:
        fail(f"baseline {baseline} is not an ancestor of {audited}")
    if compared:
        compared_ancestor = subprocess.run(
            ["git", "-C", str(upstream), "merge-base", "--is-ancestor", audited, compared]
        )
        if compared_ancestor.returncode != 0:
            fail(f"audited_upstream {audited} is not an ancestor of compared_against {compared}")

    actual = git(
        upstream,
        "log",
        "--reverse",
        "--format=%H",
        f"{baseline}..{audited}",
        "--",
        *paths,
    ).splitlines()
    if actual != classified:
        missing = [commit for commit in actual if commit not in seen]
        extra = [commit for commit in classified if commit not in set(actual)]
        order_mismatch = not missing and not extra
        details = []
        if missing:
            details.append("unclassified=" + ",".join(missing))
        if extra:
            details.append("outside-range=" + ",".join(extra))
        if order_mismatch:
            details.append("entries are not in upstream chronological order")
        fail("audit entries do not match mapped upstream history: " + "; ".join(details))


def index_trailers(repo: Path):
    history = git(repo, "log", "--all", "--format=%H%x1f%ct%x1f%B%x1e")
    origin_to_local = {}
    for record in history.split("\x1e"):
        record = record.strip()
        if not record or record.count("\x1f") < 2:
            continue
        local_commit, ct_raw, body = record.split("\x1f", 2)
        try:
            local_ct = int(ct_raw)
        except ValueError:
            fail(f"invalid committer timestamp on {local_commit}")
        for line in body.splitlines():
            if line.startswith("Zed-Origin: "):
                origin = line.removeprefix("Zed-Origin: ").strip()
                origin_to_local.setdefault(origin, []).append((local_ct, local_commit))
    return origin_to_local


def collect_extra_backports(upstream, supplemental, post_audit, backports):
    for label, extra in (
        ("supplemental_backports", supplemental),
        ("post_audit_backports", post_audit),
    ):
        if extra is None:
            extra = []
        if not isinstance(extra, list):
            fail(f"{label} must be an array")
        for entry in extra:
            if not isinstance(entry, dict) or not SHA_RE.fullmatch(entry.get("commit", "")):
                fail(f"{label} entries must use full commit hashes")
            note = entry.get("note", "")
            if not isinstance(note, str) or not note.strip():
                fail(f"{label} {entry.get('commit')} must explain the backport")
            commit = entry["commit"]
            git(upstream, "cat-file", "-e", f"{commit}^{{commit}}")
            backports.append(commit)


def committer_unix(cwd: Path, commit: str) -> int:
    raw = git(cwd, "log", "-1", "--format=%ct", commit).strip()
    try:
        return int(raw)
    except ValueError:
        fail(f"could not read committer timestamp for {commit}")


def verify_latest_backports(upstream, traced, origin_to_local, newest_by_date, newest_by_trailer):
    if newest_by_date:
        if newest_by_date not in traced:
            fail(
                f"newest_ported_by_date {newest_by_date} is not listed as a "
                "backport / supplemental / post-audit SHA"
            )
        expected_by_date = max(
            traced, key=lambda origin: (committer_unix(upstream, origin), origin)
        )
        if newest_by_date != expected_by_date:
            fail(
                f"newest_ported_by_date {newest_by_date} is not the newest traced "
                "backport by upstream committer date; "
                f"expected {expected_by_date} (tie-break: later date, then larger SHA)"
            )

    if newest_by_trailer:
        if newest_by_trailer not in traced:
            fail(
                f"newest_ported_by_trailer {newest_by_trailer} is not listed as a "
                "backport / supplemental / post-audit SHA"
            )
        expected_by_trailer = None
        expected_key = None
        for origin in traced:
            matches = origin_to_local.get(origin, [])
            if not matches:
                continue
            local_ct, local_sha = max(matches)
            key = (local_ct, local_sha, origin)
            if expected_key is None or key > expected_key:
                expected_key = key
                expected_by_trailer = origin
        if expected_by_trailer is None:
            fail("newest_ported_by_trailer is set but no traced backport has a local Zed-Origin")
        if newest_by_trailer != expected_by_trailer:
            fail(
                f"newest_ported_by_trailer {newest_by_trailer} is not the traced "
                "backport with the newest local Zed-Origin commit; "
                f"expected {expected_by_trailer} "
                "(tie-break: later local date, then larger local SHA, then larger origin SHA)"
            )


def main() -> None:
    repo = Path(sys.argv[1]).resolve()
    upstream = Path(sys.argv[2]).resolve()
    audit_path = Path(sys.argv[3]).resolve()
    audit = read_audit(audit_path, upstream)
    supplemental = audit.get("supplemental_backports", [])
    post_audit = audit.get("post_audit_backports", [])

    def optional_sha(audit: dict, label: str) -> str | None:
        value = audit.get(label)
        if value is None:
            return None
        return require_sha(label, value)

    baseline, audited, paths, entries, seen, classified, backports = validate_audit(audit)
    compared = optional_sha(audit, "compared_against")
    newest_by_date = optional_sha(audit, "newest_ported_by_date")
    newest_by_trailer = optional_sha(audit, "newest_ported_by_trailer")
    verify_range(upstream, baseline, audited, compared, newest_by_date, newest_by_trailer,
                 paths, classified, seen)
    origin_to_local = index_trailers(repo)
    collect_extra_backports(upstream, supplemental, post_audit, backports)
    verify_latest_backports(upstream, set(backports), origin_to_local,
                            newest_by_date, newest_by_trailer)
    for origin in backports:
        if not origin_to_local.get(origin, []):
            fail(f"backport {origin} has no exact Zed-Origin trailer in local history")

    print(
        "upstream sync verification passed: "
        f"{len(entries)} classified commits, {len(backports)} traced backports, "
        f"{len(post_audit) if isinstance(post_audit, list) else 0} post-audit cherry-picks, "
        f"range {baseline[:10]}..{audited[:10]}"
    )


if __name__ == "__main__":
    main()
