#!/bin/sh
# Approach A probe — report the HOST's pending package updates from inside a
# container, using read-only mounts of the host's apt and dpkg state.
#
# Emits influx line protocol on stdout, for Telegraf's inputs.exec with
# data_format = "influx". Exits 0 even when the check fails: a failed check is
# data (check_success=0), not a crash — Telegraf would otherwise just log an
# error and emit nothing, which is indistinguishable from the module being off.
#
# THE INVARIANT: if the host's package state cannot be read, this reports
# check_success=0 and OMITS the pending counts. It never reports zero.
# "No updates pending" and "I could not look" are opposite conclusions.
#
# Usage:  HOSTFS=/hostfs ./probe.sh
#
# Requires apt-get and dpkg in the container running it.

set -u

HOSTFS="${HOSTFS:-/hostfs}"
MEASUREMENT="muninn_updates"

DPKG_STATUS="$HOSTFS/var/lib/dpkg/status"
APT_ETC="$HOSTFS/etc/apt"
APT_LISTS="$HOSTFS/var/lib/apt/lists"
OS_RELEASE="$HOSTFS/etc/os-release"

# Report a failed check and stop. `reason` is a short stable token, not a free
# text message — it becomes a metric tag, so it must stay low-cardinality and
# must never carry a path or an error string.
fail() {
    echo "${MEASUREMENT},status=error,reason=$1 check_success=0i"
    exit 0
}

# ── Preconditions ────────────────────────────────────────────────────────────
# Checked one at a time so the reason is specific. Every one of these is a
# deployment mistake that would otherwise produce a plausible wrong number.

[ -d "$HOSTFS" ]          || fail hostfs_not_mounted
[ -r "$DPKG_STATUS" ]     || fail dpkg_status_unreadable
[ -s "$DPKG_STATUS" ]     || fail dpkg_status_empty
[ -d "$APT_ETC" ]         || fail apt_etc_missing
[ -d "$APT_LISTS" ]       || fail apt_lists_missing

# An apt lists directory with no *_Packages index means `apt-get update` has
# never run on the host, or the mount points somewhere unexpected. Without
# indices apt reports zero pending updates — correctly, from its point of view,
# and completely misleadingly.
if [ -z "$(find "$APT_LISTS" -maxdepth 1 -name '*_Packages*' -print -quit 2>/dev/null)" ]; then
    fail apt_lists_empty
fi

# Debian family only. A host running something else would otherwise get a
# confident answer derived from a package manager it does not use.
#
# Both locations are tried because /etc/os-release is a SYMLINK to
# ../usr/lib/os-release on Debian and Ubuntu. A deployment that mounts /etc but
# not /usr/lib leaves that symlink dangling, and reading only the /etc path
# would report "not a Debian host" for a machine that plainly is. This is one of
# the concrete reasons the documented mount is the whole root rather than a
# hand-picked list of paths (ADR-0005).
if [ -r "$OS_RELEASE" ]; then
    os_file="$OS_RELEASE"
elif [ -r "$HOSTFS/usr/lib/os-release" ]; then
    os_file="$HOSTFS/usr/lib/os-release"
else
    fail os_release_unreadable
fi

host_id=$(sed -n 's/^ID=//p' "$os_file" | tr -d '"' | head -1)
host_like=$(sed -n 's/^ID_LIKE=//p' "$os_file" | tr -d '"' | head -1)
case " $host_id $host_like " in
    *debian*|*ubuntu*) : ;;
    *) fail host_not_debian_family ;;
esac

# ── Run the simulated upgrade ────────────────────────────────────────────────
# Every apt directory that could be written to is redirected into a scratch
# directory inside the container. The host paths above are only ever read, which
# is why the mount can be — and in the documented deployment is — read-only.

CACHE=$(mktemp -d) || fail scratch_unavailable
# shellcheck disable=SC2064  # expand CACHE now, not at trap time
trap "rm -rf '$CACHE'" EXIT
mkdir -p "$CACHE/archives/partial"

out="$CACHE/upgrade.txt"
err="$CACHE/upgrade.err"

# -s              simulate; resolve and print, change nothing
# NoLocking       do not try to take /var/lib/dpkg/lock — the mount is read-only
# Dir::Cache      the one directory apt genuinely writes to
apt-get -s dist-upgrade \
    -o Dir::State::status="$DPKG_STATUS" \
    -o Dir::Etc::sourcelist="$APT_ETC/sources.list" \
    -o Dir::Etc::sourceparts="$APT_ETC/sources.list.d" \
    -o Dir::Etc::trusted="$APT_ETC/trusted.gpg" \
    -o Dir::Etc::trustedparts="$APT_ETC/trusted.gpg.d" \
    -o Dir::Etc::preferences="$APT_ETC/preferences" \
    -o Dir::Etc::preferencesparts="$APT_ETC/preferences.d" \
    -o Dir::State::lists="$APT_LISTS" \
    -o Dir::Cache="$CACHE" \
    -o Debug::NoLocking=1 \
    -o APT::Get::Show-Versions=false \
    > "$out" 2> "$err"

rc=$?
if [ "$rc" -ne 0 ]; then
    # A non-zero apt is not "zero updates". Most often the host's package index
    # format is newer than this container's apt understands (see T10).
    fail apt_failed
fi

# ── Count ────────────────────────────────────────────────────────────────────
# apt prints one "Inst <pkg> [old] (new <Origin>:<suite> ...)" line per package
# it would install or upgrade.

total=$(grep -c '^Inst ' "$out")

# Security updates are identified by the origin of the candidate version, which
# apt prints in the parenthesised part of each Inst line: Debian writes
# "Debian-Security:12/stable-security", Ubuntu "Ubuntu:24.04/noble-security".
# Matching on "-security" rather than the vendor name keeps both working, and
# keeps working for a third-party security suite.
security=$(grep '^Inst ' "$out" | grep -c -- '-[Ss]ecurity')

# Sanity: security can never exceed the total. If it does, the parse is wrong
# and reporting either number would be worse than reporting the failure.
if [ "$security" -gt "$total" ]; then
    fail parse_inconsistent
fi

# ── Age of the package lists ─────────────────────────────────────────────────
# Not a failure condition: stale lists still give a correct answer about a stale
# picture. Reported so an alert can distinguish "no updates" from "nobody has
# run apt-get update since March".

now=$(date +%s)
newest=$(find "$APT_LISTS" -maxdepth 1 -name '*_Packages*' -printf '%T@\n' 2>/dev/null \
         | sort -rn | head -1 | cut -d. -f1)
if [ -n "$newest" ]; then
    lists_age=$(( now - newest ))
    [ "$lists_age" -lt 0 ] && lists_age=0
else
    lists_age=-1
fi

echo "${MEASUREMENT},status=ok pending_all=${total}i,pending_security=${security}i,check_success=1i,lists_age_seconds=${lists_age}i,check_timestamp_seconds=${now}i"
