#!/usr/bin/env sh
# Ground truth for the security subset: how many of the packages named on
# stdin have a candidate version available from a security origin.
#
# Reads package names on stdin, one per line. Prints one number.
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# The ground truth used to be `apt-get -s dist-upgrade | grep -c -- '-security'`
# — the origin apt prints on each `Inst` line. On Debian that is accurate. On
# Ubuntu it is a lower bound, because Ubuntu publishes a security update to
# `<release>-security` *and* copies it into `<release>-updates`; apt names only
# the pocket it resolved through. The same fixture measured 66 pending / 34
# security when it was built and 66 pending / 0 security when rebuilt against a
# later archive, with the packages unchanged. That was R8, and it is why muninn
# now classifies by asking `apt-cache policy` which origins the candidate
# version is available from.
#
# ── What this is, and what it is not ─────────────────────────────────────────
#
# It is a **second implementation of the same rule**, in awk, run on the host
# itself rather than through muninn. That catches a mistake in muninn's parser,
# a mistake in its apt invocation, and a drift between the two — which is what a
# ground truth in this suite is for.
#
# It is **not an independent authority**. If the rule itself is wrong, both
# implementations are wrong together. The genuinely independent check is
# Ubuntu's own `/usr/lib/update-notifier/apt-check`, which is not installed in
# the base images and cannot be installed without changing the package state
# being measured. Establishing that is recorded as the remaining work.
#
# ── The format being parsed ──────────────────────────────────────────────────
#
#   libc6:
#     Installed: 2.39-0ubuntu8.2
#     Candidate: 2.39-0ubuntu8.3
#     Version table:
#        2.39-0ubuntu8.3 500
#           500 http://archive.ubuntu.com/ubuntu noble-updates/main amd64 Packages
#           500 http://security.ubuntu.com/ubuntu noble-security/main amd64 Packages
#    *** 2.39-0ubuntu8.2 100
#           100 /var/lib/dpkg/status
#
# The second source line is the whole point: apt would print `noble-updates` on
# the `Inst` line, and the version is plainly also in `noble-security`.
#
# A version-table entry and one of its source lines are told apart by their
# first field — a source line begins with a priority number — rather than by
# indentation, which the `***` marker on the installed version shifts.

set -eu

packages=$(cat)
if [ -z "$packages" ]; then
    echo 0
    exit 0
fi

# Word splitting is wanted: apt-cache takes the names as separate arguments.
# shellcheck disable=SC2086
apt-cache policy $packages 2>/dev/null | awk '
    /^[^[:space:]].*:$/ {
        pkg = substr($0, 1, length($0) - 1)
        candidate = ""
        in_candidate = 0
        next
    }
    /^[[:space:]]*Candidate:/ { candidate = $2; next }
    NF == 0 { next }
    {
        if ($1 ~ /^[0-9]+$/) {
            # A source line for whichever version block we are in.
            if (in_candidate && tolower($0) ~ /-security/) hit[pkg] = 1
            next
        }
        # A version-table entry ends in a priority; anything else is prose.
        if (NF >= 2 && $NF ~ /^[0-9]+$/) {
            version = ($1 == "***") ? $2 : $1
            in_candidate = (version == candidate)
        }
    }
    END {
        n = 0
        for (p in hit) n++
        print n
    }
'
